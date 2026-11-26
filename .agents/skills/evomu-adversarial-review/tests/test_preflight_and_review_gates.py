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

            # Regress arch_debt_ratchet above max_occurrences and client_compat above max_rpc_rules (#1538 upper-bound)
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/market.rs"),
                "pub fn charge(w: &Wallet) { let _ = w.master_wallet + w.master_wallet; }\n",
            )
            compat_json["max_rpc_rules"] = max(0, rep["rpc_rules"] - 1)
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

    def test_coalesce_cargo_test_commands(self):
        # 1. Coalesces multiple --test it and --lib filters into one command per target,
        #    deduplicates filters in order, and keeps check / --bin / other crates separate.
        raw = [
            ["cargo", "check", "-j", "4", "--workspace", "--bins"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", "rpc_client_contracts"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", "login_space_bootstrap_sequence"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "macho_listener"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "dispatch"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "macho_listener"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", "quasar_gateway"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", "rpc_client_contracts"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--bin", "eve-server", "main_unit_tests"],
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
        self.assertEqual(
            run_review._coalesce_cargo_test_commands(raw),
            [
                ["cargo", "check", "-j", "4", "--workspace", "--bins"],
                [
                    "cargo",
                    "test",
                    "-j",
                    "4",
                    "-p",
                    "eve-server",
                    "--test",
                    "it",
                    "--",
                    "rpc_client_contracts",
                    "login_space_bootstrap_sequence",
                    "quasar_gateway",
                ],
                ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "--", "macho_listener", "dispatch"],
                ["cargo", "test", "-j", "4", "-p", "eve-server", "--bin", "eve-server", "main_unit_tests"],
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
            ],
        )

        # 2. Duplicate single filter collapses cleanly to one command without duplicating filter
        dup_single = [
            ["cargo", "test", "-j", "4", "-p", "eve-db", "--lib", "lock_rank"],
            ["cargo", "test", "-j", "4", "-p", "eve-db", "--lib", "lock_rank"],
        ]
        self.assertEqual(
            run_review._coalesce_cargo_test_commands(dup_single),
            [["cargo", "test", "-j", "4", "-p", "eve-db", "--lib", "lock_rank"]],
        )

        # 3. Unfiltered command for a target subsumes filtered commands for the same target (either order)
        subsumed = [
            ["cargo", "test", "-j", "4", "-p", "eve-proto", "--lib", "pb"],
            ["cargo", "test", "-j", "4", "-p", "eve-proto", "--lib"],
            ["cargo", "test", "-j", "4", "-p", "eve-proto", "--lib", "marshal"],
            ["cargo", "test", "-j", "4", "-p", "eve-quasar", "--lib"],
            ["cargo", "test", "-j", "4", "-p", "eve-quasar", "--lib", "framing"],
        ]
        self.assertEqual(
            run_review._coalesce_cargo_test_commands(subsumed),
            [
                ["cargo", "test", "-j", "4", "-p", "eve-proto", "--lib"],
                ["cargo", "test", "-j", "4", "-p", "eve-quasar", "--lib"],
            ],
        )

        # 4. Commands with extra flags (-- --exact, --ignored, --workspace, --no-fail-fast) stay unmerged
        specials = [
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "foo"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "--", "--exact", "exact_fn"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "--ignored"],
            ["cargo", "test", "-j", "4", "--workspace", "--no-fail-fast"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "bar"],
        ]
        self.assertEqual(
            run_review._coalesce_cargo_test_commands(specials),
            [
                ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "--", "foo", "bar"],
                ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "--", "--exact", "exact_fn"],
                ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "--ignored"],
                ["cargo", "test", "-j", "4", "--workspace", "--no-fail-fast"],
            ],
        )

        # 5. End-to-end check inside `run_tests`: multiple affected commands coalesce into one per target
        seen_cmds = []

        def fake_sh(cmd, cwd, env=None, timeout=None):
            seen_cmds.append(list(cmd))
            return 0, "test demo ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured\n"

        multi_derived = [
            ["cargo", "test", "-p", "eve-server", "--test", "it", "rpc_client_contracts"],
            ["cargo", "test", "-p", "eve-server", "--test", "it", "client_harness"],
            ["cargo", "test", "-p", "eve-server", "--lib", "macho_listener"],
            ["cargo", "test", "-p", "eve-server", "--lib", "router::binding"],
            ["cargo", "test", "-p", "eve-server", "--bin", "eve-server", "main_unit_tests"],
        ]
        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.object(run_review, "sh", side_effect=fake_sh), mock.patch.object(
                run_review, "_acquire_cargo_test_slot", return_value=None
            ), mock.patch.object(run_review, "derive_affected_test_commands", return_value=multi_derived):
                _, meta = run_review.run_tests(REPO_ROOT, tmp, timeout=30, base="HEAD")
        self.assertEqual(meta["exit"], 0)
        self.assertEqual(
            [c[1:] for c in seen_cmds],
            [
                ["check", "-j", "4", "--workspace", "--bins"],
                ["test", "-j", "4", "-p", "eve-server", "--test", "it", "--", "rpc_client_contracts", "client_harness"],
                ["test", "-j", "4", "-p", "eve-server", "--lib", "--", "macho_listener", "router::binding"],
                ["test", "-j", "4", "-p", "eve-server", "--bin", "eve-server", "main_unit_tests"],
            ],
        )

    def test_acquire_cargo_test_slot_respects_global_active_cargo_cap(self):
        # Verify `_count_active_cargo_processes` parsing, Playtester exemption, parent-PID exclusion,
        # and non-fail-open `None` return on pgrep error (TT-22).
        def _fake_pgrep_only(cmd, **kwargs):
            if cmd[:2] == ["pgrep", "-x"]:
                return subprocess.CompletedProcess(cmd, 0, "101\n202\n303\n", "")
            return subprocess.CompletedProcess(cmd, 1, "", "")

        with mock.patch.object(run_review.subprocess, "run", side_effect=_fake_pgrep_only):
            self.assertEqual(run_review._count_active_cargo_processes(), 3)

        # Exempt Playtester / hourly_test_suite builds and children of held slot PIDs are excluded
        def _fake_pgrep_with_meta(cmd, **kwargs):
            if cmd[:2] == ["pgrep", "-x"]:
                return subprocess.CompletedProcess(cmd, 0, "101\n202\n303\n404\n", "")
            if cmd[0] == "ps":
                return subprocess.CompletedProcess(
                    cmd,
                    0,
                    "101 9001 cargo test -p eve-server\n"
                    "202 8888 cargo build --manifest-path /tmp/EvoMu-playtester/Cargo.toml\n"
                    "303 7777 cargo test -p eve-server\n"
                    "404 6666 cargo test -p eve-db\n",
                    "",
                )
            if cmd[0] == "lsof":
                return subprocess.CompletedProcess(
                    cmd,
                    0,
                    "p101\nn/work/wt-1\np202\nn/work/EvoMu-playtester\np303\nn/work/wt-2\np404\nn/work/wt-3\n",
                    "",
                )
            return subprocess.CompletedProcess(cmd, 1, "", "")

        with mock.patch.object(run_review.subprocess, "run", side_effect=_fake_pgrep_with_meta):
            # 202 is exempt Playtester; 303 is a child of held slot PID 7777 -> only 101 & 404 count!
            self.assertEqual(run_review._count_active_cargo_processes(exclude_parent_pids={7777}), 2)

        with mock.patch.object(
            run_review.subprocess,
            "run",
            return_value=subprocess.CompletedProcess(["pgrep", "-x", "cargo"], 1, "", ""),
        ):
            self.assertEqual(run_review._count_active_cargo_processes(), 0)
        with mock.patch.object(run_review.subprocess, "run", side_effect=FileNotFoundError("no pgrep")):
            self.assertIsNone(run_review._count_active_cargo_processes())

        # Verify `_acquire_cargo_test_slot` waits when held_slots + external_cargo >= max_slots,
        # uses coordinator mutex to avoid TOCTOU overshoot, and acquires cleanly once < max_slots.
        sleeps = []
        counts = iter([3, 4, 2])
        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.dict(os.environ, {"EVOMU_MAX_CARGO_TEST_SLOTS": "3"}), mock.patch.object(
                run_review.rl, "main_state_dir", return_value=tmp
            ), mock.patch.object(
                run_review, "_count_active_cargo_processes", side_effect=lambda **kw: next(counts)
            ), mock.patch.object(
                run_review.time, "sleep", side_effect=lambda s: sleeps.append(s)
            ):
                fh = run_review._acquire_cargo_test_slot(REPO_ROOT)
                try:
                    self.assertIsNotNone(fh)
                    self.assertEqual(sleeps, [5.0, 5.0])
                    slot0 = os.path.join(tmp, "cargo_test_slot_0.lock")
                    self.assertTrue(os.path.isfile(slot0))
                    with open(slot0, encoding="utf-8") as sfh:
                        meta = json.loads(sfh.read())
                    self.assertEqual(meta["slot"], 0)
                    self.assertEqual(meta["pid"], os.getpid())
                finally:
                    run_review._release_cargo_test_slot(fh)

                # After release, slot 0 is immediately acquirable again when active cargo < max_slots.
                with mock.patch.object(run_review, "_count_active_cargo_processes", return_value=1):
                    fh2 = run_review._acquire_cargo_test_slot(REPO_ROOT)
                    try:
                        self.assertIsNotNone(fh2)
                        with open(os.path.join(tmp, "cargo_test_slot_0.lock"), encoding="utf-8") as sfh2:
                            meta2 = json.loads(sfh2.read())
                        self.assertEqual(meta2["slot"], 0)
                    finally:
                        run_review._release_cargo_test_slot(fh2)

            # Verify default EVOMU_MAX_CARGO_TEST_SLOTS is 4 when env var is unset: 4 waits, 3 acquires.
            default_sleeps = []
            default_counts = iter([4, 3])
            clean_env = {k: v for k, v in os.environ.items() if k != "EVOMU_MAX_CARGO_TEST_SLOTS"}
            with mock.patch.dict(os.environ, clean_env, clear=True), mock.patch.object(
                run_review.rl, "main_state_dir", return_value=tmp
            ), mock.patch.object(
                run_review, "_count_active_cargo_processes", side_effect=lambda **kw: next(default_counts)
            ), mock.patch.object(
                run_review.time, "sleep", side_effect=lambda s: default_sleeps.append(s)
            ):
                fh3 = run_review._acquire_cargo_test_slot(REPO_ROOT)
                try:
                    self.assertIsNotNone(fh3)
                    self.assertEqual(default_sleeps, [5.0])
                finally:
                    run_review._release_cargo_test_slot(fh3)

            # When pgrep fails (`None`), still enforces strict kernel flock cap without crashing or failing open.
            with mock.patch.dict(os.environ, {"EVOMU_MAX_CARGO_TEST_SLOTS": "2"}), mock.patch.object(
                run_review.rl, "main_state_dir", return_value=tmp
            ), mock.patch.object(
                run_review, "_count_active_cargo_processes", return_value=None
            ):
                fha = run_review._acquire_cargo_test_slot(REPO_ROOT)
                try:
                    self.assertIsNotNone(fha)
                finally:
                    run_review._release_cargo_test_slot(fha)

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
                if os.path.basename(cmd[0]) == "cargo":
                    self.assertEqual(cmd[:4], ["cargo", "test", "-j", "4"], "%s cmd missing -j 4: %s" % (issue, cmd))

            always_or_surgical_it = (
                set(run_review.SENTINEL_IT_FILTERS)
                | set(run_review.SENTINEL_IT_CANARY_TESTS)
                | set(run_review.PATH_TRIGGERED_IT_CANARIES)
                | {"quasar_gateway", "rpc_dispatch_and_client_harness_e2e"}
            )
            non_sentinel_it = [
                c[-1]
                for c in cmds
                if "--test" in c
                and c[c.index("--test") + 1] == "it"
                and c[-1] not in always_or_surgical_it
            ]
            direct_it_changed = set()
            if has_commit:
                di_out = subprocess.run(
                    ["git", "diff", "--name-only", sha + "^1", sha, "--", "crates/eve-server/tests/it/"],
                    cwd=REPO_ROOT,
                    capture_output=True,
                    text=True,
                ).stdout
                direct_it_changed = {
                    os.path.splitext(os.path.basename(l.strip()))[0]
                    for l in di_out.splitlines()
                    if l.strip().endswith(".rs")
                }
            max_expected_it = max(
                run_review.MAX_AFFECTED_IT_FILTERS,
                len(direct_it_changed) + max(0, run_review.MAX_AFFECTED_IT_FILTERS - 4),
            )
            self.assertLessEqual(
                len(non_sentinel_it),
                max_expected_it,
                "%s exceeded <= %d integration filter cap: %s" % (issue, max_expected_it, non_sentinel_it),
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

    def test_tt01_tt02_honest_tests_and_real_unclosed_rows_enforcement(self):
        # TT-02 / PROC-03: run_review.py must NOT contain synthetic CONCURRENT_TEST_GATE fabrication
        rr_src = open(os.path.join(SCRIPTS_DIR, "run_review.py"), encoding="utf-8").read()
        self.assertNotIn("executed=CONCURRENT_TEST_GATE", rr_src)
        self.assertNotIn("test %s ... ok", rr_src)
        self.assertIn("assume_tests_pass=False", rr_src)
        self.assertNotIn("assume_tests_pass=not final_run", rr_src)

        # TT-01: _compute_unclosed_rows enforces real executed parity on BOTH scoped diff rows and full runs
        pre_done = {"rpc:marketProxy.PlaceBuyOrder", "feat:mining.belt-ore-distribution", "feat:unrelated.row"}
        real_rows_scoped = {
            "rpc:marketProxy.PlaceBuyOrder": {"status": "DONE"},
            "feat:mining.belt-ore-distribution": {"status": "TEST_NOT_PASSING"},
            "feat:unrelated.row": {"status": "TEST_NOT_PASSING"},
        }
        diff_rows = {"rpc:marketProxy.PlaceBuyOrder", "feat:mining.belt-ore-distribution"}
        # Scoped run fails ONLY on diff-tagged unclosed row, not untouched unrelated rows
        self.assertEqual(
            run_review._compute_unclosed_rows(pre_done, real_rows_scoped, diff_rows, scoped=True),
            ["feat:mining.belt-ore-distribution"],
        )
        # Full-workspace run checks every pre_test_done_row
        self.assertEqual(
            run_review._compute_unclosed_rows(pre_done, real_rows_scoped, diff_rows, scoped=False),
            ["feat:mining.belt-ore-distribution", "feat:unrelated.row"],
        )

        # _collect_diff_parity_rows finds `// parity:` tags on newly added or modified test fns in a temp git repo
        with tempfile.TemporaryDirectory() as tmp:
            subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp, check=True)
            subprocess.run(["git", "config", "user.email", "t@evomu.local"], cwd=tmp, check=True)
            subprocess.run(["git", "config", "user.name", "T"], cwd=tmp, check=True)
            _write(
                os.path.join(tmp, "crates/eve-server/tests/it/demo.rs"),
                "// parity: feat:old.untouched\n#[test]\nfn old_t() { assert_eq!(1, 1); }\n",
            )
            subprocess.run(["git", "add", "."], cwd=tmp, check=True)
            subprocess.run(["git", "commit", "-q", "-m", "base"], cwd=tmp, check=True)
            base_sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=tmp, capture_output=True, text=True, check=True).stdout.strip()
            _write(
                os.path.join(tmp, "crates/eve-server/tests/it/demo.rs"),
                "// parity: feat:old.untouched\n#[test]\nfn old_t() { assert_eq!(1, 1); }\n\n"
                "// parity: rpc:marketProxy.PlaceBuyOrder, feat:new.row\n#[test]\nfn new_t() { assert_eq!(2, 2); }\n\n"
                "// parity: feat:async.row\n#[tokio::test]\nasync fn new_async_t() { assert_eq!(3, 3); }\n",
            )
            # Moving/adding `// parity:` comments on non-test production module headers or `pub async fn`
            # (e.g. during monolith splits like pve_exploration.rs -> pve_exploration_sites.rs) must NOT enter
            # `_collect_diff_parity_rows`.
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/pve_exploration_sites.rs"),
                "//! Exploration site helpers.\n"
                "// parity: feat:pve.wormhole-sites\n\n"
                "// parity: feat:pve.combat-anomalies\n"
                "pub async fn advance_site_wave() -> bool { true }\n",
            )
            got_rows = run_review._collect_diff_parity_rows(tmp, base_sha)
            self.assertEqual(got_rows, {"rpc:marketProxy.PlaceBuyOrder", "feat:new.row", "feat:async.row"})
            self.assertNotIn("feat:pve.wormhole-sites", got_rows)
            self.assertNotIn("feat:pve.combat-anomalies", got_rows)
            # Also verify committed diffs (`head != "HEAD"`) exclude production comments and include test blocks
            subprocess.run(["git", "add", "."], cwd=tmp, check=True)
            subprocess.run(["git", "commit", "-q", "-m", "split"], cwd=tmp, check=True)
            head_sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=tmp, capture_output=True, text=True, check=True).stdout.strip()
            got_committed = run_review._collect_diff_parity_rows(tmp, base_sha, head=head_sha)
            self.assertEqual(got_committed, {"rpc:marketProxy.PlaceBuyOrder", "feat:new.row", "feat:async.row"})
            self.assertNotIn("feat:pve.wormhole-sites", got_committed)
            self.assertNotIn("feat:pve.combat-anomalies", got_committed)

    def test_tt03_core_services_df_over_45_and_no_direct_truncation(self):
        # 1. Core EVE service token (df > 45, e.g. charUnboundMgr / invbroker / corpRegistry) must NOT get dropped
        def fake_core_service_diff(cmd, cwd, env=None, timeout=None):
            if cmd[:3] == ["git", "diff", "--name-only"]:
                return 0, "crates/eve-server/src/services/custom_core_helper.rs\n"
            if cmd[:3] == ["git", "diff", "-U0"]:
                return 0, '+++ b/crates/eve-server/src/services/custom_core_helper.rs\n@@ -1 +1 @@\n+let svc = "invbroker";\n'
            return 0, ""

        with mock.patch.object(run_review, "sh", side_effect=fake_core_service_diff):
            cmds = run_review.derive_affected_test_commands(REPO_ROOT, "origin/main", "cargo", head="FAKE_SHA")
        always_or_surgical = set(run_review.SENTINEL_IT_FILTERS) | set(run_review.SENTINEL_IT_CANARY_TESTS)
        scored_it = [
            c[-1] for c in cmds
            if "--test" in c and c[c.index("--test") + 1] == "it" and c[-1] not in always_or_surgical
        ]
        self.assertGreater(len(scored_it), 0, "Core service `invbroker` (df > 45) selected 0 IT modules!")

        # 2. >4 directly modified tests/it/*.rs and >6 directly modified --lib files must NEVER be truncated,
        #    and non-server root integration test must run unfiltered (no hardcoded `"smoke"`).
        direct_its = [
            "economy_market",
            "economy_contracts_and_market",
            "economy_industry_pi",
            "social_fleets",
            "space_mining",
            "crimewatch_killrights_timers_e2e",
        ]
        direct_libs = [
            "alpha_mod",
            "beta_mod",
            "gamma_mod",
            "delta_mod",
            "epsilon_mod",
            "zeta_mod",
            "eta_mod",
            "theta_mod",
        ]
        paths = (
            ["crates/eve-server/tests/it/%s.rs" % m for m in direct_its]
            + ["crates/eve-server/src/services/%s.rs" % m for m in direct_libs]
            + ["crates/eve-proto/tests/marshal_roundtrip.rs"]
        )

        def fake_many_diff(cmd, cwd, env=None, timeout=None):
            if cmd[:3] == ["git", "diff", "--name-only"]:
                return 0, "\n".join(paths) + "\n"
            if cmd[:3] == ["git", "diff", "-U0"]:
                return 0, ""
            return 0, ""

        with mock.patch.object(run_review, "sh", side_effect=fake_many_diff):
            cmds2 = run_review.derive_affected_test_commands(REPO_ROOT, "origin/main", "cargo", head="HEAD")
        for m in direct_its:
            self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", m], cmds2)
        for m in direct_libs:
            self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", m], cmds2)
        self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-proto", "--test", "marshal_roundtrip"], cmds2)
        self.assertNotIn(["cargo", "test", "-j", "4", "-p", "eve-proto", "--test", "marshal_roundtrip", "smoke"], cmds2)

    def test_tt04_base_freshness_and_trial_merge_conflict_detection(self):
        with tempfile.TemporaryDirectory() as tmp:
            subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp, check=True)
            subprocess.run(["git", "config", "user.email", "t@evomu.local"], cwd=tmp, check=True)
            subprocess.run(["git", "config", "user.name", "T"], cwd=tmp, check=True)
            _write(os.path.join(tmp, "shared.txt"), "line1\n")
            subprocess.run(["git", "add", "."], cwd=tmp, check=True)
            subprocess.run(["git", "commit", "-q", "-m", "base"], cwd=tmp, check=True)
            subprocess.run(["git", "checkout", "-q", "-b", "feat/work"], cwd=tmp, check=True)
            _write(os.path.join(tmp, "shared.txt"), "branch-version\n")
            subprocess.run(["git", "add", "."], cwd=tmp, check=True)
            subprocess.run(["git", "commit", "-q", "-m", "branch edit"], cwd=tmp, check=True)
            # Advance main with a conflicting edit to shared.txt
            subprocess.run(["git", "checkout", "-q", "main"], cwd=tmp, check=True)
            _write(os.path.join(tmp, "shared.txt"), "main-version\n")
            subprocess.run(["git", "add", "."], cwd=tmp, check=True)
            subprocess.run(["git", "commit", "-q", "-m", "main edit"], cwd=tmp, check=True)
            subprocess.run(["git", "checkout", "-q", "feat/work"], cwd=tmp, check=True)

            info = run_review.check_base_freshness(tmp, "HEAD")
            self.assertEqual(info["commits_behind"], 1)
            self.assertEqual(info["commits_ahead"], 1)
            self.assertTrue(info["merge_conflict"])
            self.assertIn("shared.txt", info["overlapping_files"])
            self.assertIn("shared.txt", info["conflict_files"])
            # Must not touch working tree
            status = subprocess.run(["git", "status", "--porcelain"], cwd=tmp, capture_output=True, text=True, check=True).stdout
            self.assertEqual(status.strip(), "")

    def test_tt06_python_unittest_selection_and_in_place_archiving(self):
        # 1. Pure `tools/client-harness/` changes trigger Python unittest discovery ONLY (ZERO `cargo test` commands!)
        def fake_harness_diff(cmd, cwd, env=None, timeout=None):
            if cmd[:3] == ["git", "diff", "--name-only"]:
                return 0, "tools/client-harness/playtester.py\ntools/client-harness/scenarios/combat_aftermath.json\n"
            return 0, ""

        with mock.patch.object(run_review, "sh", side_effect=fake_harness_diff):
            cmds = run_review.derive_affected_test_commands(REPO_ROOT, "origin/main", "cargo", head="FAKE_SHA")
        self.assertEqual(
            cmds,
            [[sys.executable, "-m", "unittest", "discover", "-s", "tools/client-harness", "-p", "test_*.py", "-v"]],
        )
        self.assertEqual(
            [c for c in cmds if os.path.basename(c[0]) == "cargo"],
            [],
            "Pure tools/client-harness/ diff must emit 0 cargo test commands",
        )

        # 1b. Pure docs/non-Rust diff emits ZERO `cargo test` commands (runs lightweight Python release_guard unittest)
        def fake_docs_diff(cmd, cwd, env=None, timeout=None):
            return 0, ""

        with mock.patch.object(run_review, "sh", side_effect=fake_docs_diff):
            dcmds = run_review.derive_affected_test_commands(REPO_ROOT, "origin/main", "cargo", head="FAKE_SHA")
        self.assertEqual([c for c in dcmds if os.path.basename(c[0]) == "cargo"], [])
        self.assertEqual(
            dcmds,
            [[sys.executable, "-m", "unittest", "discover", "-s", "tools/tests", "-p", "test_release_guard.py", "-v"]],
        )

        # 1c. Workspace Cargo.toml / Cargo.lock / .cargo/* AND crates/eve-server/* diffs still emit all 12 surgical canaries!
        for rust_path in ("Cargo.toml", "Cargo.lock", ".cargo/config.toml", "crates/eve-server/src/services/market_orders.rs"):
            def fake_rust_diff(cmd, cwd, env=None, timeout=None, _p=rust_path):
                if cmd[:3] == ["git", "diff", "--name-only"]:
                    return 0, _p + "\n"
                return 0, ""

            with mock.patch.object(run_review, "sh", side_effect=fake_rust_diff):
                rcmds = run_review.derive_affected_test_commands(REPO_ROOT, "origin/main", "cargo", head="FAKE_SHA")
            for cflt in run_review.SENTINEL_IT_CANARY_TESTS:
                self.assertIn(
                    ["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", cflt],
                    rcmds,
                    "%s must emit IT canary %s" % (rust_path, cflt),
                )
            for cflt in run_review.SENTINEL_LIB_CANARY_TESTS:
                self.assertIn(
                    ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", cflt],
                    rcmds,
                    "%s must emit lib canary %s" % (rust_path, cflt),
                )

        # 2. Python-only `tools/sync_bugs_to_github.py` runs `tools/tests` WITHOUT acquiring a cargo slot!
        def fake_tools_diff(cmd, cwd, env=None, timeout=None):
            if cmd[:3] == ["git", "diff", "--name-only"]:
                return 0, "tools/sync_bugs_to_github.py\n"
            return 0, ""

        with mock.patch.object(run_review, "sh", side_effect=fake_tools_diff):
            tcmds = run_review.derive_affected_test_commands(REPO_ROOT, "origin/main", "cargo", head="FAKE_SHA")
        self.assertEqual(
            tcmds,
            [[sys.executable, "-m", "unittest", "discover", "-s", "tools/tests", "-p", "test_*.py", "-v"]],
        )

        def fake_py_run(cmd, cwd, env=None, timeout=None):
            return 0, "Ran 58 tests in 1.234s\n\nOK\n"

        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.object(run_review, "sh", side_effect=fake_py_run), mock.patch.object(
                run_review, "derive_affected_test_commands", return_value=tcmds
            ), mock.patch.object(run_review, "_acquire_cargo_test_slot") as mock_slot:
                out_txt, meta = run_review.run_tests(REPO_ROOT, tmp, timeout=30, base="HEAD")
            mock_slot.assert_not_called()
            self.assertEqual(meta["passed"], 58)
            self.assertEqual(meta["failed"], 0)
            self.assertTrue(meta["scoped"])
            self.assertIn("scoped=true", out_txt)

        # 3. Direct `--in-place` `record()` archives via `rl.archive_review` (TT-06)
        rr_src = open(os.path.join(SCRIPTS_DIR, "run_review.py"), encoding="utf-8").read()
        self.assertIn("if not a.from_snapshot:", rr_src)
        self.assertIn("rl.archive_review(root, rec, run_dir)", rr_src)

    def test_tt09_tt10_tt11_arch_debt_custom_scanners_exact_and_stringly_bypass(self):
        with tempfile.TemporaryDirectory() as tmp:
            # Verify all 3 formerly-skipped custom rules (TT-09) + exact unconditional check (TT-10)
            # + uncompiled rule hard failure (TT-09)
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/gm.rs"),
                'pub fn parse(cmd: &str) { let _ = cmd.strip_prefix("/help"); }\n'
                'pub fn bp(x: &S) { let _ = x.ballpark.write(); }\n',
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/system_actor_loop.rs"),
                "pub fn ok(x: &S) { let _ = x.ballpark.write(); }\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/router/binding.rs"),
                "pub fn allocate_transition_grace_binding_for_departed_kinds() {}\n"
                "pub fn grace_bound_object_for_inflight_invbroker() {}\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/arch_debt_ratchet.json"),
                json.dumps({
                    "version": 1,
                    "patterns": {
                        "slash_command_prefix_parse": {"max_occurrences": 1, "max_files": 1, "strip_literals": False},
                        "router_binding_grace_helpers": {"max_occurrences": 1, "max_files": 1, "path_include": "router/binding.rs"},
                        "ballpark_locks_outside_system_actor": {"max_occurrences": 1, "max_files": 1, "path_exclude_substring": "system_actor"},
                    },
                }),
            )
            self.assertEqual(ratchet_checks.check_arch_debt_ratchet(tmp), [])

            # Regressing each of the 3 non-regex rules MUST fail
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/gm.rs"),
                'pub fn parse(cmd: &str) { let _ = cmd.strip_prefix("/help"); let _ = cmd.starts_with("/who"); }\n'
                'pub fn bp(x: &S) { let _ = x.ballpark.write(); let _ = x.ballpark.read(); }\n',
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/router/binding.rs"),
                "pub fn grace_bound_object_for_inflight_a() {}\n"
                "pub fn has_transition_grace_b_binding() {}\n",
            )
            viols = ratchet_checks.check_arch_debt_ratchet(tmp)
            v_txt = "\n".join(v[1] for v in viols)
            self.assertIn("slash_command_prefix_parse", v_txt)
            self.assertIn("router_binding_grace_helpers", v_txt)
            self.assertIn("ballpark_locks_outside_system_actor", v_txt)

            # TT-09: Unknown rule without regex/literals/custom scanner MUST fail hard (never silently skipped!)
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/arch_debt_ratchet.json"),
                json.dumps({"version": 1, "patterns": {"unwired_mystery_rule": {"max_occurrences": 5, "max_files": 2}}}),
            )
            unwired = ratchet_checks.check_arch_debt_ratchet(tmp)
            self.assertEqual(len(unwired), 1)
            self.assertIn("unwired_mystery_rule", unwired[0][1])

            # #1538: zero_required rules stay strictly pinned at 0 (v2 rejects non-empty baseline or any occurrence),
            # while lowering a non-zero per-file count passes without editing the baseline!
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/arch_debt_ratchet.json"),
                json.dumps({
                    "version": 2,
                    "patterns": {
                        "master_wallet": {"zero_required": True, "regex": r"\bmaster_wallet\b", "files": {"services/gm.rs": 1}},
                    },
                }),
            )
            zero_viols = ratchet_checks.check_arch_debt_ratchet(tmp)
            self.assertTrue(any("zero_required=true" in v[1] for v in zero_viols), zero_viols)

            # TT-11: Companion stringly BridgeRpcError bypass ratchet catches const ERR_*, non-literal code,
            #        helper defs, and helper calls!
            _write(
                os.path.join(tmp, ratchet_checks.STRINGLY_ERR_BYPASS_REL),
                json.dumps({
                    "version": 1,
                    "owning_issue": 1195,
                    "rules": {
                        "const_err_str_defs": {"max_occurrences": 0, "max_files": 0},
                        "bridge_rpc_error_nonlit_code": {"max_occurrences": 0, "max_files": 0},
                        "bridge_rpc_error_helper_defs": {"max_occurrences": 0, "max_files": 0},
                        "bridge_rpc_error_helper_calls": {"max_occurrences": 0, "max_files": 0},
                    },
                }),
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/sneaky.rs"),
                'const ERR_SNEAKY: &str = "Sneaky";\n'
                'fn my_err(code: &str, msg: &str) -> BridgeRpcError {\n'
                '    BridgeRpcError { code: code.into(), message: msg.into(), client_error: None }\n'
                '}\n'
                'pub fn run() -> Result<(), BridgeRpcError> {\n'
                '    Err(my_err(ERR_SNEAKY, "nope"))\n'
                '}\n',
            )
            b_viols = ratchet_checks.check_stringly_error_bypass_ratchet(tmp)
            b_txt = "\n".join(v[1] for v in b_viols)
            for metric in (
                "const_err_str_defs",
                "bridge_rpc_error_nonlit_code",
                "bridge_rpc_error_helper_defs",
                "bridge_rpc_error_helper_calls",
            ):
                self.assertIn(metric, b_txt, "Missing TT-11 metric %s in %s" % (metric, b_viols))

    def test_tt23_anchored_test_summary_parser_and_pending_gap_audit_guard(self):
        # Arbitrary stdout containing "3 failed" MUST NOT corrupt `failed` count (TT-23)
        raw_out = (
            "running 2 tests\n"
            "WARN: 3 failed login attempts retried cleanly\n"
            "test alpha ... ok\n"
            "test beta ... ok\n"
            "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
            "Ran 10 tests in 0.123s\n\nOK (skipped=1)\n"
        )
        passed, failed = run_review._parse_test_counts(raw_out)
        self.assertEqual((passed, failed), (11, 0))

        fail_out = (
            "test result: FAILED. 4 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out\n"
            "Ran 8 tests in 0.456s\n\nFAILED (failures=1, errors=2)\n"
        )
        p2, f2 = run_review._parse_test_counts(fail_out)
        self.assertEqual((p2, f2), (9, 5))

        # background_gap_audit refuses provisional/pending test_meta (TT-23)
        with tempfile.TemporaryDirectory() as tmp:
            run_dir = os.path.join(tmp, "20261120-000000")
            ctx = os.path.join(run_dir, "context")
            os.makedirs(ctx, exist_ok=True)
            _write(os.path.join(ctx, "run_meta.json"), json.dumps({
                "ts": "20261120-000000", "head": "abc", "base": "def", "tree": "123",
                "test_meta": {"pending": True, "executed": False},
            }))
            fake_args = mock.Mock(reuse_context=run_dir, model="auto", effort="medium", reviewer_timeout="0s", launcher="agy")
            self.assertEqual(run_review.background_gap_audit(fake_args, tmp, tmp), 3)

    def test_proc01_rubric_enforcement_and_proc12_increment_cap(self):
        # Own-diff P1 (explicit scope) -> promoted to FAIL
        so1 = {
            "verdict": "PASS_WITH_ISSUES",
            "summary": "ok",
            "claims_assessment": [],
            "blocking_findings": [{
                "id": "F-1", "severity": "P1", "scope": "own_diff",
                "title": "Missing auth check", "evidence": "crates/eve-server/src/services/foo.rs:42", "fix": "check",
            }],
        }
        v1, r1 = run_review._enforce_verdict_rubric("PASS_WITH_ISSUES", so1, changed_files=["crates/eve-server/src/services/foo.rs"])
        self.assertEqual(v1, "FAIL", r1)

        # Unscoped P1 citing changed file -> promoted to FAIL
        so2 = {
            "verdict": "PASS_WITH_ISSUES",
            "summary": "ok",
            "claims_assessment": [],
            "blocking_findings": [{
                "id": "F-2", "severity": "P1",
                "title": "Bug in foo", "evidence": "foo.rs:99", "fix": "fix",
            }],
        }
        v2, r2 = run_review._enforce_verdict_rubric("PASS_WITH_ISSUES", so2, changed_files=["crates/eve-server/src/services/foo.rs"])
        self.assertEqual(v2, "FAIL", r2)

        # Flaky/failing existing-test finding -> promoted to FAIL even if labeled pre_existing (#1370)
        so3 = {
            "verdict": "PASS_WITH_ISSUES",
            "summary": "ok",
            "claims_assessment": [],
            "blocking_findings": [{
                "id": "F-3", "severity": "P1", "scope": "pre_existing",
                "title": "Existing test gh998 is now flaky", "evidence": "agents_tests.rs:10", "fix": "sort",
            }],
        }
        v3, r3 = run_review._enforce_verdict_rubric("PASS_WITH_ISSUES", so3, changed_files=["crates/eve-server/src/services/other.rs"])
        self.assertEqual(v3, "FAIL", r3)

        # Strictly pre-existing non-regression P1 outside diff -> stays PASS_WITH_ISSUES
        so4 = {
            "verdict": "PASS_WITH_ISSUES",
            "summary": "ok",
            "claims_assessment": [],
            "blocking_findings": [{
                "id": "F-4", "severity": "P1", "scope": "pre_existing",
                "title": "Pre-existing tech debt in unrelated handler", "evidence": "crates/eve-server/src/services/unrelated.rs:50", "fix": "later",
            }],
        }
        v4, r4 = run_review._enforce_verdict_rubric("PASS_WITH_ISSUES", so4, changed_files=["crates/eve-server/src/services/foo.rs"])
        self.assertEqual(v4, "PASS_WITH_ISSUES", r4)

        # PROC-12: claims-only `increment-size: allow` cannot waive > 8,000 lines without EVOMU_ALLOW_LARGE_INCREMENT=1
        with tempfile.TemporaryDirectory() as tmp:
            cpath = os.path.join(tmp, "claims.md")
            _write(cpath, "increment-size: allow big refactor reason here\n")
            with mock.patch.object(preflight, "increment_size", return_value=(10, 9500)):
                clean_env = {k: v for k, v in os.environ.items() if k != "EVOMU_ALLOW_LARGE_INCREMENT"}
                with mock.patch.dict(os.environ, clean_env, clear=True):
                    self.assertEqual([v[0] for v in preflight.lint_increment_size(tmp, cpath)], ["INCREMENT_TOO_LARGE"])
                with mock.patch.dict(os.environ, {"EVOMU_ALLOW_LARGE_INCREMENT": "1"}):
                    self.assertEqual(preflight.lint_increment_size(tmp, cpath), [])

    def test_surgical_historical_canaries_and_t1_to_t6_rules(self):
        # 1. Explicit exclusions must NEVER appear in sentinels
        all_sentinels = (
            set(run_review.SENTINEL_IT_FILTERS)
            | set(run_review.SENTINEL_IT_CANARY_TESTS)
            | set(run_review.SENTINEL_LIB_CANARY_TESTS)
        )
        for forbidden in (
            "test_homefront_drifter_triglavian_and_metaliminal_storms_e2e",
            "esi_server_tcp_roundtrip_chunked_and_error_guards",
            "rpc_negative_matrix",
        ):
            self.assertFalse(any(forbidden in s for s in all_sentinels), "Forbidden sentinel: %s" % forbidden)

        # 2. Verify all 12 canary test fn names + all T1..T6 test fn names exist in HEAD crates/
        all_named_tests = (
            list(run_review.SENTINEL_IT_CANARY_TESTS)
            + list(run_review.SENTINEL_LIB_CANARY_TESTS)
            + [run_review.T1_CLIENT_CONTRACT_TEST]
            + list(run_review.T2_DESTINY_IT_TESTS)
            + list(run_review.T3_CORP_IT_TESTS)
            + list(run_review.T3_CORP_LIB_TESTS)
            + list(run_review.T4_COMBAT_MODULE_IT_TESTS)
            + list(run_review.T4_COMBAT_MODULE_LIB_TESTS)
            + list(run_review.T5_STRUCTURE_IT_TESTS)
            + [run_review.T6_PROTO_LIB_TEST]
        )
        for full_t in all_named_tests:
            fn_name = full_t.rsplit("::", 1)[-1]
            rc_g, out_g = run_review.sh(["git", "grep", "-n", "fn %s(" % fn_name, "--", "crates/"], REPO_ROOT)
            self.assertEqual(rc_g, 0, "Canary/trigger test fn `%s` (%s) not found in crates/!" % (fn_name, full_t))

        def run_for(paths, diff_u0=""):
            def fake_sh(cmd, cwd, env=None, timeout=None):
                if cmd[:3] == ["git", "diff", "--name-only"]:
                    return 0, "\n".join(paths) + "\n"
                if cmd[:3] == ["git", "diff", "-U0"]:
                    return 0, diff_u0
                return 0, ""

            with mock.patch.object(run_review, "sh", side_effect=fake_sh):
                return run_review.derive_affected_test_commands(REPO_ROOT, "origin/main", "cargo", head="FAKE_SHA")

        # 3. Any Rust change emits all 9 IT canaries + all 3 lib canaries
        basic_cmds = run_for(["crates/eve-server/src/services/market_orders.rs"])
        for cflt in run_review.SENTINEL_IT_CANARY_TESTS:
            self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", cflt], basic_cmds)
        for cflt in run_review.SENTINEL_LIB_CANARY_TESTS:
            self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", cflt], basic_cmds)

        # 4. T1..T6 path-triggered rules
        # T1 via client_error.rs and via BridgeRpcError diff
        t1_cmds = run_for(["crates/eve-proto/src/client_error.rs"])
        self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-testkit", "--lib", run_review.T1_CLIENT_CONTRACT_TEST], t1_cmds)
        t1_diff_cmds = run_for(
            ["crates/eve-server/src/services/market_orders.rs"],
            diff_u0="+++ b/crates/eve-server/src/services/market_orders.rs\n@@ -1 +1 @@\n+Err(BridgeRpcError::new())\n",
        )
        self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-testkit", "--lib", run_review.T1_CLIENT_CONTRACT_TEST], t1_diff_cmds)

        # T2 via crates/eve-destiny/ and space_warp_jump.rs
        t2_cmds = run_for(["crates/eve-server/src/services/space_warp_jump.rs"])
        self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-destiny", "--lib"], t2_cmds)
        self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-destiny", "--test", "t11_destiny_differential"], t2_cmds)
        for tflt in run_review.T2_DESTINY_IT_TESTS:
            self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", tflt], t2_cmds)

        # T3 via crates/eve-db/src/characters.rs and corp_governance.rs
        t3_cmds = run_for(["crates/eve-db/src/characters.rs"])
        for tflt in run_review.T3_CORP_IT_TESTS:
            self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", tflt], t3_cmds)
        for tflt in run_review.T3_CORP_LIB_TESTS:
            self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", tflt], t3_cmds)

        # T4 via fitting_module_cycles.rs and eve-proto/src/marshal.rs
        for t4_p in ("crates/eve-server/src/services/fitting_module_cycles.rs", "crates/eve-proto/src/marshal.rs"):
            t4_cmds = run_for([t4_p])
            for tflt in run_review.T4_COMBAT_MODULE_IT_TESTS:
                self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", tflt], t4_cmds)
            for tflt in run_review.T4_COMBAT_MODULE_LIB_TESTS:
                self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", tflt], t4_cmds)

        # T5 via structure_upwell_ops.rs
        t5_cmds = run_for(["crates/eve-server/src/services/structure_upwell_ops.rs"])
        for tflt in run_review.T5_STRUCTURE_IT_TESTS:
            self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", tflt], t5_cmds)

        # T6 via quasar_gateway.rs
        t6_cmds = run_for(["crates/eve-server/src/quasar_gateway.rs"])
        self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-proto", "--lib", run_review.T6_PROTO_LIB_TEST], t6_cmds)

        # Test-only files must NOT spuriously fire T3/T4/T5
        t_only_cmds = run_for(["crates/eve-server/src/services/structure_upwell_ops_tests.rs"])
        for tflt in run_review.T5_STRUCTURE_IT_TESTS:
            self.assertNotIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", tflt], t_only_cmds)

    def test_tt24_doc_policy_alignment(self):
        # SKILL.md and AGENTS.md must align with final_gate.py (no stale "primary model only" or Opus 5.5 High claim)
        skill_txt = open(os.path.join(SKILL_DIR, "SKILL.md"), encoding="utf-8").read()
        agents_txt = open(os.path.join(REPO_ROOT, "AGENTS.md"), encoding="utf-8").read()
        self.assertNotIn("The final gate still requires a PASS from the primary model", skill_txt)
        self.assertNotIn("The final gate still needs a PASS from the primary model", agents_txt)
        self.assertNotIn("Opus 5.5 High", agents_txt)
        self.assertIn("EVOMU_FINAL_REVIEW_CHAIN", skill_txt)
        self.assertIn("EVOMU_FINAL_REVIEW_CHAIN", agents_txt)
        self.assertIn("tools/client-harness/", skill_txt)
        self.assertIn("tools/client-harness/", agents_txt)

    def test_1538_per_file_ratchets_auto_tighten_and_no_nonzero_equality(self):
        import tighten_ratchets  # noqa: E402

        # 1. Acceptance criterion: zero non-zero equality comparisons remain in ratchet_checks.py
        with open(os.path.join(SCRIPTS_DIR, "ratchet_checks.py"), encoding="utf-8") as fh:
            rc_src = fh.read()
        self.assertNotIn("dropped below exact ratchet", rc_src)
        self.assertNotIn("dropped below falling ratchet", rc_src)
        self.assertNotIn("without removing from", rc_src)

        with tempfile.TemporaryDirectory() as tmp:
            # Build minimal synthetic repo with v2 arch_debt, v2 client_compat, v2 untyped_user_error, v2 stringly_bypass
            _write(os.path.join(tmp, "crates/eve-server/src/router/client_compat.rs"), _minimal_client_compat_rs())
            _write(
                os.path.join(tmp, "crates/eve-server/src/outbound_sequencer.rs"),
                "pub fn restamp_frame() { let mut a = [0u8; 4]; a.copy_from_slice(&[1,2,3,4]); }\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/alpha.rs"),
                "pub fn a(x: &mut Map) {\n"
                "  x.items.insert(1, 2);\n"
                "  let _ = BridgeRpcError::user_error(\"A\", \"msg\");\n"
                "  const ERR_A: &str = \"A\";\n"
                "}\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/beta.rs"),
                "pub fn b(x: &mut Map) {\n"
                "  x.items.insert(3, 4);\n"
                "  x.items.insert(5, 6);\n"
                "  let _ = BridgeRpcError::user_error(\"B\", \"msg\");\n"
                "}\n",
            )
            # Start with v1 templates, migrate via migrate_v2 -> exact v2 baselines
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/arch_debt_ratchet.json"),
                json.dumps({
                    "version": 1,
                    "patterns": {
                        "master_wallet": {"max_occurrences": 0, "max_files": 0, "zero_required": True, "regex": r"\bmaster_wallet\b"},
                        "items_insert": {"max_occurrences": 3, "max_files": 2, "owning_issue": 1194, "regex": r"\bitems\s*\.\s*insert\s*\("},
                    },
                }),
            )
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/client_compat_ratchet.json"),
                json.dumps({"version": 1, "max_transport_differing_replies": 7}),
            )
            _write(
                os.path.join(tmp, ratchet_checks.STRINGLY_ERR_BYPASS_REL),
                json.dumps({"version": 1, "owning_issue": 1195, "rules": {}}),
            )
            written = tighten_ratchets.migrate_v2(tmp)
            self.assertEqual(len(written), 4)
            self.assertEqual(ratchet_checks.check_all_ratchets(tmp, cache_path=os.path.join(tmp, "c.json")), [])

            # Verify sequencer_set_state_rewrites was counted (1 restamp_ fn + 1 copy_from_slice = 2)
            cc_v2 = ratchet_checks._read_json(os.path.join(tmp, "crates/eve-testkit/ratchets/client_compat_ratchet.json"))
            self.assertEqual(cc_v2["version"], 2)
            self.assertEqual(
                cc_v2["counters"]["sequencer_set_state_rewrites"],
                {"crates/eve-server/src/outbound_sequencer.rs": 2},
            )
            self.assertEqual(cc_v2["max_transport_differing_replies"], 7)

            # 2. Two branches each removing different sites (alpha removes items.insert & user_error; beta lowers items.insert 2->1)
            #    WITHOUT editing any baseline JSON -> passes cleanly with zero violations!
            _write(os.path.join(tmp, "crates/eve-server/src/services/alpha.rs"), "pub fn a() {}\n")
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/beta.rs"),
                "pub fn b(x: &mut Map) {\n"
                "  x.items.insert(3, 4);\n"
                "  let _ = BridgeRpcError::user_error(\"B\", \"msg\");\n"
                "}\n",
            )
            self.assertEqual(ratchet_checks.check_all_ratchets(tmp, cache_path=os.path.join(tmp, "c.json")), [])

            # 3. Auto-tightener lowers beta (2->1) and deletes alpha (1->0) across all affected v2 files mechanically!
            changed, slack, regs = tighten_ratchets.tighten_ratchets(tmp, write=True)
            self.assertEqual(regs, [])
            self.assertGreater(slack, 0)
            self.assertIn("crates/eve-testkit/ratchets/arch_debt_ratchet.json", changed)
            self.assertIn("crates/eve-testkit/ratchets/untyped_user_error_ratchet.json", changed)
            self.assertIn(ratchet_checks.STRINGLY_ERR_BYPASS_REL, changed)

            ad_after = ratchet_checks._read_json(os.path.join(tmp, "crates/eve-testkit/ratchets/arch_debt_ratchet.json"))
            self.assertEqual(ad_after["patterns"]["items_insert"]["files"], {"services/beta.rs": 1})
            ue_after = ratchet_checks._read_json(os.path.join(tmp, "crates/eve-testkit/ratchets/untyped_user_error_ratchet.json"))
            self.assertEqual(ue_after["files"], {"services/beta.rs": 1})
            # Re-checking after tightening shows 0 slack
            changed2, slack2, regs2 = tighten_ratchets.tighten_ratchets(tmp, write=False)
            self.assertEqual((changed2, slack2, regs2), ([], 0, []))

            # 4. Adding a site back to an unlisted file (alpha.rs, implicit baseline 0) fails hard,
            #    and tighten_ratchets refuses to raise baselines!
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/alpha.rs"),
                "pub fn a(x: &mut Map) { x.items.insert(9, 9); let _ = BridgeRpcError::user_error(\"Z\", \"m\"); }\n",
            )
            viols = ratchet_checks.check_all_ratchets(tmp, cache_path=os.path.join(tmp, "c.json"))
            vcats = {v[0] for v in viols}
            self.assertIn("RATCHET_ARCH_DEBT", vcats)
            self.assertIn("RATCHET_UNTYPED_USER_ERROR", vcats)
            _, _, regs3 = tighten_ratchets.tighten_ratchets(tmp, write=True)
            self.assertGreaterEqual(len(regs3), 2)
            # Baseline must NOT have been raised by tighten_ratchets
            ad_still = ratchet_checks._read_json(os.path.join(tmp, "crates/eve-testkit/ratchets/arch_debt_ratchet.json"))
            self.assertEqual(ad_still["patterns"]["items_insert"]["files"], {"services/beta.rs": 1})

        # 5. Live repo verification: dump-counts-json & check_all_ratchets & stringly_error_bypass v2 tight
        live_counts = tighten_ratchets.collect_all_counts(REPO_ROOT)
        self.assertGreaterEqual(len(live_counts["arch_debt"]), 19)
        self.assertEqual(set(live_counts["client_compat"]["counters"].keys()), set(ratchet_checks.CLIENT_COMPAT_COUNTER_KEYS))
        self.assertGreater(sum(live_counts["untyped_user_error"].values()), 0)
        self.assertEqual(set(live_counts["test_fixture"].keys()), set(ratchet_checks.TEST_FIXTURE_PATTERNS))
        self.assertEqual(ratchet_checks.check_all_ratchets(REPO_ROOT), [])
        _, sb_slack, sb_regs = tighten_ratchets.tighten_ratchets(REPO_ROOT, write=False, targets=("stringly_error_bypass",))
        self.assertEqual((sb_slack, sb_regs), (0, []))

    def test_1540_test_fixture_ratchet_parity_enforcement_and_tightening(self):
        import tighten_ratchets  # noqa: E402

        # 1. Exact scanner parity on clean & dirty snippets mirroring crates/eve-testkit/src/test_fixture_ratchet.rs
        clean_src = (
            "// SessionState { comment }\n"
            "/* SessionState { nested /* comment */ } let _ = dispatch_rpc(); */\n"
            "// let _ = ctx.dispatch_rpc(&mut s, \"svc\", None, \"M\", &[], &kw, None).await;\n"
            "let text = \"SessionState { literal } let _ = dispatch_rpc();\";\n"
            "let mut sess = SessionState::new(100);\n"
            "let rep = harness.call_ok(\"account\", None, \"GetCashBalance\", &[], &kw).await;\n"
            "let _unused_local = 42;\n"
            "let _ = std::fs::remove_file(\"/tmp/none\");\n"
            "let _ = plain_non_method_call(1);\n"
            "let _ = crate::call(1);\n"
        )
        self.assertEqual(ratchet_checks.count_hand_built_sessions(clean_src), 0)
        self.assertEqual(ratchet_checks.count_discarded_rpc_results(clean_src), 0)

        dirty_src = (
            "let s1 = SessionState { session_id: 1, ..Default::default() };\n"
            "let s2 = SessionState { session_id: 2, ..Default::default() };\n"
            "let _ = ctx.dispatch_rpc(&mut sess, \"a\", None, \"B\", &[], &kw, None).await;\n"
            "let _ = ctx.dispatch_rpc(&mut sess, \"a\", None, \"C\", &[], &kw, None).await.expect(\"C\");\n"
            "let _ = harness.call(\"a\", None, \"D\", &[], &kw).await;\n"
            "let _ = call_rpc(&ctx, &mut sess, \"a\", \"E\", &[]).await;\n"
            "let _ = gm.call_wire(\"a\", None, \"F\", &[], &kw).await;\n"
            "let _act_rep = harness.call_ok(\"dogmaIM\", None, \"Activate\", &[], &kw).await;\n"
            "let _lock_rep = wire_session_call_ok(&mut s, \"target\", \"AddTarget\", &[]).await;\n"
        )
        self.assertEqual(ratchet_checks.count_hand_built_sessions(dirty_src), 2)
        self.assertEqual(ratchet_checks.count_discarded_rpc_results(dirty_src), 7)

        # Function/closure return types (`-> SessionState {`, multiline, path-qualified) must NOT count as
        # hand-built struct literals, while actual `SessionState { ... }` literals in every expression position DO.
        return_type_only_src = (
            "pub(crate) async fn login_demo(ctx: &ServerContext) -> SessionState {\n"
            "    SessionState::new(1)\n"
            "}\n"
            "fn multiline(\n"
            "    ctx: &ServerContext,\n"
            ") ->\n"
            "    SessionState\n"
            "{\n"
            "    SessionState::new(2)\n"
            "}\n"
            "fn qualified() -> eve_server::router::SessionState {\n"
            "    eve_server::router::SessionState::new(3)\n"
            "}\n"
            "fn global_path() -> ::eve_server::SessionState {\n"
            "    SessionState::new(4)\n"
            "}\n"
            "fn with_comment() -> /* return type */ SessionState {\n"
            "    SessionState::new(5)\n"
            "}\n"
            "let mk = |id: u64| -> SessionState { SessionState::new(id) };\n"
        )
        self.assertEqual(ratchet_checks.count_hand_built_sessions(return_type_only_src), 0)

        mixed_return_and_literals_src = (
            "fn make_sess(id: u64) -> SessionState {\n"
            "    SessionState { session_id: id, ..Default::default() }\n"
            "}\n"
            "fn more_literals(flag: bool) -> eve_server::router::SessionState {\n"
            "    let a = SessionState { session_id: 1 };\n"
            "    let b = Some(eve_server::router::SessionState { session_id: 2 });\n"
            "    let c = vec![a, SessionState { session_id: 3 }];\n"
            "    let d = match flag {\n"
            "        true => SessionState { session_id: 4 },\n"
            "        false => return SessionState { session_id: 5 },\n"
            "    };\n"
            "    let _ = (b, c);\n"
            "    d\n"
            "}\n"
        )
        self.assertEqual(ratchet_checks.count_hand_built_sessions(mixed_return_and_literals_src), 6)

        # Edge cases: nested let _ = inside block counts once (advances past statement); unmatched `}` stops stmt;
        # whitespace before `.call_ok(` and `rpc`/`rpc_res`/`call_exchange` all match.
        edge_src = (
            "fn f() {\n"
            "    let _ = { let _ = dispatch_rpc(); };\n"
            "    if true { let _ = rpc_res() }\n"
            "    let _x = h . call_ok (1);\n"
            "    let _y = call_exchange(1);\n"
            "    let _z = rpc(1);\n"
            "}\n"
        )
        self.assertEqual(ratchet_checks.count_discarded_rpc_results(edge_src), 5)

        # 2. Synthetic repo: per-file enforcement, umbrella protection, auto-tightening, and regression refusal
        with tempfile.TemporaryDirectory() as tmp:
            a_rel = "crates/eve-server/tests/it/alpha.rs"
            b_rel = "crates/eve-server/tests/it/sub/beta.rs"
            _write(
                os.path.join(tmp, a_rel),
                "fn t() {\n"
                "  let s = SessionState { session_id: 1 };\n"
                "  let _ = harness.call_ok(\"svc\", None, \"M\", &[], &kw).await;\n"
                "}\n",
            )
            _write(
                os.path.join(tmp, b_rel),
                "fn t() {\n"
                "  let s1 = SessionState { session_id: 1 };\n"
                "  let s2 = SessionState { session_id: 2 };\n"
                "  let _ = ctx.dispatch_rpc(&mut s1, \"a\", None, \"B\", &[], &kw, None).await;\n"
                "  let _ = call_rpc(&ctx, &mut s2, \"a\", \"C\", &[]).await;\n"
                "}\n",
            )
            _write(
                os.path.join(tmp, ratchet_checks.TEST_FIXTURE_REL),
                json.dumps({
                    "version": 2,
                    "owning_issue": "#1540",
                    "description": "test",
                    "patterns": {
                        "hand_built_sessions": {"rule": "r1", "zero_required": False, "files": {a_rel: 1, b_rel: 2}},
                        "discarded_rpc_results": {"rule": "r2", "zero_required": False, "files": {a_rel: 1, b_rel: 2}},
                    },
                }),
            )
            self.assertEqual(ratchet_checks.check_all_ratchets(tmp, cache_path=os.path.join(tmp, "c.json")), [])
            self.assertIn(1540, ratchet_checks.protected_umbrella_issues(tmp))
            self.assertEqual(
                tighten_ratchets.tighten_ratchets(tmp, write=False, targets=("test_fixture",)),
                ([], 0, []),
            )

            # Removing debt without touching JSON passes cleanly (no non-zero equality pin)
            _write(os.path.join(tmp, a_rel), "fn t() { let _ = SessionState::new(1); }\n")
            _write(
                os.path.join(tmp, b_rel),
                "fn t() {\n"
                "  let s1 = SessionState { session_id: 1 };\n"
                "  let _ = ctx.dispatch_rpc(&mut s1, \"a\", None, \"B\", &[], &kw, None).await;\n"
                "}\n",
            )
            self.assertEqual(ratchet_checks.check_all_ratchets(tmp, cache_path=os.path.join(tmp, "c.json")), [])

            # Auto-tighten strictly lowers beta (2->1) and deletes alpha (1->0)
            changed, slack, regs = tighten_ratchets.tighten_ratchets(tmp, write=True, targets=("test_fixture",))
            self.assertEqual(regs, [])
            self.assertEqual(slack, 4)
            self.assertEqual(changed, [ratchet_checks.TEST_FIXTURE_REL])
            tf_after = ratchet_checks._read_json(os.path.join(tmp, ratchet_checks.TEST_FIXTURE_REL))
            self.assertEqual(tf_after["patterns"]["hand_built_sessions"]["files"], {b_rel: 1})
            self.assertEqual(tf_after["patterns"]["discarded_rpc_results"]["files"], {b_rel: 1})
            self.assertEqual(
                tighten_ratchets.tighten_ratchets(tmp, write=False, targets=("test_fixture",)),
                ([], 0, []),
            )

            # Adding debt to an unlisted file or increasing beta fails hard, and tighten_ratchets refuses to raise
            _write(os.path.join(tmp, a_rel), "fn t() { let s = SessionState { session_id: 9 }; }\n")
            _write(
                os.path.join(tmp, b_rel),
                "fn t() {\n"
                "  let s1 = SessionState { session_id: 1 };\n"
                "  let _ = ctx.dispatch_rpc(&mut s1, \"a\", None, \"B\", &[], &kw, None).await;\n"
                "  let _ = harness.call(\"a\", None, \"C\", &[], &kw).await;\n"
                "}\n",
            )
            viols = ratchet_checks.check_all_ratchets(tmp, cache_path=os.path.join(tmp, "c.json"))
            self.assertTrue(any(v[0] == "RATCHET_TEST_FIXTURE" and a_rel in v[1] for v in viols), viols)
            self.assertTrue(any(v[0] == "RATCHET_TEST_FIXTURE" and b_rel in v[1] for v in viols), viols)
            _, _, regs2 = tighten_ratchets.tighten_ratchets(tmp, write=True, targets=("test_fixture",))
            self.assertEqual(len(regs2), 2)
            tf_still = ratchet_checks._read_json(os.path.join(tmp, ratchet_checks.TEST_FIXTURE_REL))
            self.assertEqual(tf_still["patterns"]["hand_built_sessions"]["files"], {b_rel: 1})
            self.assertEqual(tf_still["patterns"]["discarded_rpc_results"]["files"], {b_rel: 1})

            # Once all files reach 0 and tighten, #1540 is automatically unprotected
            _write(os.path.join(tmp, a_rel), "fn t() {}\n")
            _write(os.path.join(tmp, b_rel), "fn t() {}\n")
            tighten_ratchets.tighten_ratchets(tmp, write=True, targets=("test_fixture",))
            self.assertNotIn(1540, ratchet_checks.protected_umbrella_issues(tmp))

        # 3. Live repo verification (upper-bound safe so parallel #1540 cleanup tracks never trip false equality)
        live_doc = ratchet_checks._read_json(os.path.join(REPO_ROOT, ratchet_checks.TEST_FIXTURE_REL))
        self.assertEqual(ratchet_checks.check_test_fixture_ratchet(REPO_ROOT), [])
        _, _, tf_regs = tighten_ratchets.tighten_ratchets(REPO_ROOT, write=False, targets=("test_fixture",))
        self.assertEqual(tf_regs, [])
        if any(sum(live_doc["patterns"][p]["files"].values()) > 0 for p in ratchet_checks.TEST_FIXTURE_PATTERNS):
            self.assertIn(1540, ratchet_checks.protected_umbrella_issues(REPO_ROOT))
        # Also confirm --dump-counts-json CLI preserves existing keys and exposes `test_fixture`
        dump_proc = subprocess.run(
            [sys.executable, os.path.join(SCRIPTS_DIR, "tighten_ratchets.py"), "--root", REPO_ROOT, "--dump-counts-json"],
            capture_output=True,
            text=True,
            check=True,
        )
        dumped = json.loads(dump_proc.stdout)
        self.assertTrue(
            {
                "arch_debt",
                "client_compat",
                "untyped_user_error",
                "stringly_error_bypass",
                "test_fixture",
                "system_lifecycle",
            }.issubset(dumped.keys())
        )
        self.assertEqual(set(dumped["test_fixture"].keys()), set(ratchet_checks.TEST_FIXTURE_PATTERNS))
        self.assertEqual(set(dumped["system_lifecycle"].keys()), set(ratchet_checks.SYSTEM_LIFECYCLE_RULES))

    def test_1542_system_lifecycle_ratchet_parity_enforcement_and_tightening(self):
        import tighten_ratchets  # noqa: E402

        # 1. Disguised-call parity mirroring crates/eve-testkit/src/system_lifecycle_ratchet.rs:473-550
        with tempfile.TemporaryDirectory() as tmp:
            _write(
                os.path.join(tmp, "crates/eve-server/src/mod_a.rs"),
                '// ctx.systems.get_or_spawn(30_000_142);\n'
                '/* ctx.systems.get_or_spawn_if_runtime(30_000_142); */\n'
                'pub fn get_or_spawn(&self, id: u32) {}\n'
                'pub fn get_or_spawn_if_runtime(&self, id: u32) {}\n'
                'pub fn disguised_callers(ctx: &Ctx, sys_id: u32) {\n'
                '    let _s1 = "ctx.systems.get_or_spawn(sys_id)";\n'
                '    let _s2 = r#"ctx.systems.get_or_spawn_if_runtime(sys_id)"#;\n'
                '    ctx.systems\n'
                '        .get_or_spawn\n'
                '        (sys_id);\n'
                '    ctx.systems.get_or_spawn_if_runtime /* inline comment */ (sys_id);\n'
                '    SystemRegistry::get_or_spawn(&ctx.systems, sys_id);\n'
                '    (SystemRegistry::get_or_spawn_if_runtime)(&ctx.systems, sys_id);\n'
                '    ctx.systems.get_or_spawn::<fn() -> ()>(sys_id);\n'
                '}\n'
                '#[cfg(test)]\n'
                'mod tests {\n'
                '    #[test]\n'
                '    fn test_only_spawn() { ctx.systems.get_or_spawn(30_000_142); }\n'
                '}\n',
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/mod_a_tests.rs"),
                "fn t() { ctx.systems.get_or_spawn(1); }\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/helper_caller.rs"),
                'use crate::services::quasar_payment_store_types::ensure_store_catalog_seeded;\n'
                'pub async fn ensure_system_pve_sites_seeded() {}\n'
                '// ensure_rookie_overview_seeded(ctx);\n'
                'pub async fn handler(ctx: &Ctx, sys_id: u32) {\n'
                '    let _lit = "ensure_corp_rec_ads_populated_in_snap(snap)";\n'
                '    Box::pin(ensure_system_pve_sites_seeded /* sneaky */ (ctx, sys_id)).await;\n'
                '    crate::services::pve_homefront_sites::ensure_homefront_sites_for_system_locked\n'
                '        (ctx, reg, sys_id);\n'
                '    (ensure_client_flatbuffer_storage_seeded)("id", "v1", b"1");\n'
                '}\n'
                '#[cfg(test)]\n'
                'fn cfg_test_helper(ctx: &Ctx) { ensure_special_sites_initialized(ctx); }\n',
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/helper_caller_tests.rs"),
                "fn t() { ensure_store_catalog_seeded(snap, sde); }\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/outside_services.rs"),
                "fn startup(snap: &mut Snap, sde: &Sde) { ensure_store_catalog_seeded(snap, sde); }\n",
            )

            obs = ratchet_checks.scan_system_lifecycle_per_file(tmp)
            self.assertEqual(obs[ratchet_checks.SYSTEM_LIFECYCLE_RULE_GET_OR_SPAWN], {"mod_a.rs": 5})
            self.assertEqual(obs[ratchet_checks.SYSTEM_LIFECYCLE_RULE_SEED_IF_MISSING], {"services/helper_caller.rs": 3})

            sl_path = os.path.join(tmp, ratchet_checks.SYSTEM_LIFECYCLE_REL)
            _write(
                sl_path,
                json.dumps({
                    "version": 2,
                    "description": "synthetic #1542 ratchet",
                    "rules": {
                        ratchet_checks.SYSTEM_LIFECYCLE_RULE_GET_OR_SPAWN: {
                            "zero_required": False,
                            "owning_issue": 1542,
                            "regex": r"\bget_or_spawn(?:_if_runtime)?\s*\(",
                            "description": "spawn rule",
                            "files": {"mod_a.rs": 5},
                        },
                        ratchet_checks.SYSTEM_LIFECYCLE_RULE_SEED_IF_MISSING: {
                            "zero_required": False,
                            "owning_issue": 1542,
                            "path_include": "services/",
                            "regex": "synthetic",
                            "description": "seed rule",
                            "files": {"services/helper_caller.rs": 3},
                        },
                    },
                }),
            )
            self.assertEqual(ratchet_checks.check_all_ratchets(tmp, cache_path=os.path.join(tmp, "c.json")), [])
            self.assertIn(1542, ratchet_checks.protected_umbrella_issues(tmp))
            self.assertEqual(
                tighten_ratchets.tighten_ratchets(tmp, write=False, targets=("system_lifecycle",)),
                ([], 0, []),
            )

            # Reduction passes without touching JSON; auto-tightener strictly lowers and preserves schema
            _write(os.path.join(tmp, "crates/eve-server/src/mod_a.rs"), "pub fn f(c: &Ctx) { c.get_or_spawn(1); }\n")
            _write(os.path.join(tmp, "crates/eve-server/src/services/helper_caller.rs"), "pub fn h() {}\n")
            self.assertEqual(ratchet_checks.check_all_ratchets(tmp, cache_path=os.path.join(tmp, "c.json")), [])
            changed, slack, regs = tighten_ratchets.tighten_ratchets(tmp, write=True, targets=("system_lifecycle",))
            self.assertEqual(regs, [])
            self.assertEqual(slack, 2)
            self.assertEqual(changed, [ratchet_checks.SYSTEM_LIFECYCLE_REL])
            after = ratchet_checks._read_json(sl_path)
            self.assertEqual(set(after.keys()), {"version", "description", "rules"})
            self.assertEqual(after["rules"][ratchet_checks.SYSTEM_LIFECYCLE_RULE_GET_OR_SPAWN]["files"], {"mod_a.rs": 1})
            self.assertEqual(after["rules"][ratchet_checks.SYSTEM_LIFECYCLE_RULE_SEED_IF_MISSING]["files"], {})

            # Regression (+1 in existing file + unlisted services/ file) fails hard; tighten_ratchets refuses to raise
            _write(
                os.path.join(tmp, "crates/eve-server/src/mod_a.rs"),
                "pub fn f(c: &Ctx) { c.get_or_spawn(1); c.get_or_spawn_if_runtime(2); }\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/new_svc.rs"),
                "pub fn g(s: &Sde) { ensure_sde_initialized(s); }\n",
            )
            viols = ratchet_checks.check_all_ratchets(tmp, cache_path=os.path.join(tmp, "c.json"))
            self.assertTrue(any(v[0] == "RATCHET_SYSTEM_LIFECYCLE" and "mod_a.rs" in v[1] for v in viols), viols)
            self.assertTrue(any(v[0] == "RATCHET_SYSTEM_LIFECYCLE" and "services/new_svc.rs" in v[1] for v in viols), viols)
            _, _, regs2 = tighten_ratchets.tighten_ratchets(tmp, write=True, targets=("system_lifecycle",))
            self.assertEqual(len(regs2), 2)
            still = ratchet_checks._read_json(sl_path)
            self.assertEqual(still["rules"][ratchet_checks.SYSTEM_LIFECYCLE_RULE_GET_OR_SPAWN]["files"], {"mod_a.rs": 1})

            # Once all sites hit 0 and tighten, #1542 is automatically unprotected
            _write(os.path.join(tmp, "crates/eve-server/src/mod_a.rs"), "pub fn f() {}\n")
            _write(os.path.join(tmp, "crates/eve-server/src/services/new_svc.rs"), "pub fn g() {}\n")
            tighten_ratchets.tighten_ratchets(tmp, write=True, targets=("system_lifecycle",))
            self.assertNotIn(1542, ratchet_checks.protected_umbrella_issues(tmp))

            # Unknown top-level field rejected (deny_unknown_fields Rust parity)
            bad = ratchet_checks._read_json(sl_path)
            bad["extra_field"] = True
            _write(sl_path, json.dumps(bad))
            bad_viols = ratchet_checks.check_system_lifecycle_ratchet(tmp)
            self.assertTrue(any(v[0] == "RATCHET_SYSTEM_LIFECYCLE" and "unknown top-level" in v[1] for v in bad_viols), bad_viols)

        # 2. Live repo verification & CLI exit 0
        self.assertEqual(ratchet_checks.check_system_lifecycle_ratchet(REPO_ROOT), [])
        _, _, sl_regs = tighten_ratchets.tighten_ratchets(REPO_ROOT, write=False, targets=("system_lifecycle",))
        self.assertEqual(sl_regs, [])
        cli = subprocess.run(
            [sys.executable, os.path.join(SCRIPTS_DIR, "ratchet_checks.py"), REPO_ROOT],
            capture_output=True,
            text=True,
        )
        self.assertEqual(cli.returncode, 0, cli.stdout + cli.stderr)

    def test_arch_check_33_run_review_target_dir_infra_error_retry_and_error_verdict(self):
        from unittest import mock

        # 1. Detector unit checks
        self.assertTrue(
            run_review._detect_cargo_target_infra_error(
                "error: couldn't create a temp dir: No such file or directory (os error 2)\n", 101, failed=0
            )
        )
        self.assertTrue(
            run_review._detect_cargo_target_infra_error(
                "error: failed to write `/tmp/wt/target/debug/deps/libfoo.rmeta`\nCaused by:\n  No such file or directory (os error 2)\n",
                101,
                failed=0,
            )
        )
        self.assertEqual(
            run_review._detect_cargo_target_infra_error(
                "error: couldn't read `crates/eve-server/src/missing.rs`: No such file or directory (os error 2)\n",
                101,
                failed=0,
            ),
            "",
        )
        self.assertEqual(
            run_review._detect_cargo_target_infra_error("error[E0425]: cannot find value `x` in this scope\n", 101, failed=0),
            "",
        )
        self.assertEqual(
            run_review._detect_cargo_target_infra_error(
                "error: couldn't create a temp dir: No such file or directory (os error 2)\n", 101, failed=1
            ),
            "",
        )

        # 2. End-to-end mocked run_tests + _evaluate_post_review_test_gate (ZERO real cargo runs)
        with tempfile.TemporaryDirectory() as tmp:
            sdir = os.path.join(tmp, ".evomu-reviews")
            ctx = os.path.join(sdir, "ctx")
            os.makedirs(ctx, exist_ok=True)
            fake_cmd = [["/opt/homebrew/bin/cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "foo"]]

            # 2a. Attempt 1 hits tempdir os error 2 -> automatic retry succeeds -> PASS preserved, 0 blockers
            calls_a = []
            seq_a = [
                (101, "error: couldn't create a temp dir: No such file or directory (os error 2)\n"),
                (0, "running 4 tests\ntest result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"),
            ]
            def fake_sh_a(cmd, cwd, env=None, timeout=None):
                calls_a.append(cmd)
                return seq_a[len(calls_a) - 1]

            with mock.patch.object(run_review, "derive_affected_test_commands", return_value=fake_cmd), \
                 mock.patch.object(run_review, "_acquire_cargo_test_slot", return_value=None), \
                 mock.patch.object(run_review, "_release_cargo_test_slot"), \
                 mock.patch.object(run_review, "sh", side_effect=fake_sh_a):
                text_a, tm_a = run_review.run_tests(tmp, sdir, 60, base="HEAD")
            self.assertEqual(len(calls_a), 2)
            self.assertEqual(tm_a["exit"], 0)
            self.assertEqual(tm_a["passed"], 4)
            self.assertTrue(tm_a["infra_retried"])
            self.assertFalse(tm_a["infra_error"])
            so_a = {"verdict": "PASS", "summary": "ok", "blocking_findings": [], "report_markdown": "body"}
            v_a, _, _ = run_review._evaluate_post_review_test_gate(
                tmp, "HEAD", "HEAD", ctx, text_a, tm_a, "PASS", so_a, state_path=""
            )
            self.assertEqual(v_a, "PASS")
            self.assertEqual(so_a["blocking_findings"], [])

            # 2b. Attempt 1 AND retry both hit target/ os error 2 with 0 failed tests -> ERROR (exit 3), NOT FAIL
            calls_b = []
            err_b = (101, "error: failed to open /tmp/x/target/debug/deps/eve.d: No such file or directory (os error 2)\n")
            def fake_sh_b(cmd, cwd, env=None, timeout=None):
                calls_b.append(cmd)
                return err_b

            with mock.patch.object(run_review, "derive_affected_test_commands", return_value=fake_cmd), \
                 mock.patch.object(run_review, "_acquire_cargo_test_slot", return_value=None), \
                 mock.patch.object(run_review, "_release_cargo_test_slot"), \
                 mock.patch.object(run_review, "sh", side_effect=fake_sh_b):
                text_b, tm_b = run_review.run_tests(tmp, sdir, 60, base="HEAD")
            self.assertEqual(len(calls_b), 2)
            self.assertEqual(tm_b["failed"], 0)
            self.assertTrue(tm_b["infra_retried"])
            self.assertTrue(tm_b["infra_error"])
            so_b = {"verdict": "PASS", "summary": "ok", "blocking_findings": [], "report_markdown": "body"}
            v_b, _, _ = run_review._evaluate_post_review_test_gate(
                tmp, "HEAD", "HEAD", ctx, text_b, tm_b, "PASS", so_b, state_path=""
            )
            self.assertEqual(v_b, "ERROR")
            self.assertEqual(run_review.EXIT.get(v_b, 3), 3)
            self.assertEqual(so_b["blocking_findings"], [])
            self.assertIn("Orchestrator Test Infrastructure Error", so_b["report_markdown"])

            # 2c. Real compile error is NOT retried and records FAIL + ORCHESTRATOR-POST-REVIEW-TEST-FAIL
            calls_c = []
            def fake_sh_c(cmd, cwd, env=None, timeout=None):
                calls_c.append(cmd)
                return (101, "error[E0425]: cannot find value `missing` in this scope\n")

            with mock.patch.object(run_review, "derive_affected_test_commands", return_value=fake_cmd), \
                 mock.patch.object(run_review, "_acquire_cargo_test_slot", return_value=None), \
                 mock.patch.object(run_review, "_release_cargo_test_slot"), \
                 mock.patch.object(run_review, "sh", side_effect=fake_sh_c):
                text_c, tm_c = run_review.run_tests(tmp, sdir, 60, base="HEAD")
            self.assertEqual(len(calls_c), 1)
            self.assertFalse(tm_c["infra_retried"])
            self.assertFalse(tm_c["infra_error"])
            self.assertTrue(tm_c["non_test_failure"])
            so_c = {"verdict": "PASS", "summary": "ok", "blocking_findings": [], "report_markdown": "body"}
            v_c, _, _ = run_review._evaluate_post_review_test_gate(
                tmp, "HEAD", "HEAD", ctx, text_c, tm_c, "PASS", so_c, state_path=""
            )
            self.assertEqual(v_c, "FAIL")
            self.assertEqual(so_c["blocking_findings"][0]["id"], "ORCHESTRATOR-POST-REVIEW-TEST-FAIL")

    def test_arch_check_34_run_review_known_red_main_subtraction_guards(self):
        def _git(cwd, *args):
            p = subprocess.run(["git"] + list(args), cwd=cwd, capture_output=True, text=True, check=True)
            return p.stdout.strip()

        with tempfile.TemporaryDirectory() as tmp:
            repo = os.path.join(tmp, "repo")
            ctx = os.path.join(tmp, "ctx")
            os.makedirs(repo)
            os.makedirs(ctx)
            _git(repo, "init", "-b", "main")
            _git(repo, "config", "user.email", "test@example.com")
            _git(repo, "config", "user.name", "Test")
            _write(os.path.join(repo, "a.txt"), "base\n")
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "initial commit")
            c0 = _git(repo, "rev-parse", "HEAD")

            # Advance main to c1 (red main commit) and create origin/main ref at c1
            _write(os.path.join(repo, "a.txt"), "red-main\n")
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "unrelated trunk commit")
            c1 = _git(repo, "rev-parse", "HEAD")
            _git(repo, "update-ref", "refs/remotes/origin/main", c1)

            # Create an unrelated feature branch off c1
            _git(repo, "checkout", "-b", "feat/unrelated", c1)
            _write(os.path.join(repo, "feat.txt"), "feature work\n")
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "feat: unrelated change (Refs #1540)")
            feat_head = _git(repo, "rev-parse", "HEAD")

            state_path = os.path.join(tmp, "hourly_test_suite_state.json")
            base_state = {
                "status": "FAIL",
                "main_sha": c1,
                "failed_tests": [
                    "mining_advanced::test_mn15_corp_mining_ledger_service_and_observer_tracking",
                    "router::dispatch::dispatch_unit_tests::dispatch_rpc_p0_1350_preauth_gm_role_and_moniker_gates",
                ],
                "p0_issue": {"number": 1587, "title": "[P0][Hourly CI] Full test suite breakage on main"},
            }
            _write(state_path, json.dumps(base_state))

            old_env = os.environ.get("EVOMU_HOURLY_CI_STATE_PATH")
            os.environ["EVOMU_HOURLY_CI_STATE_PATH"] = state_path
            try:
                # 1. Positive (exact + module::test suffix match): verdict stays PASS, 0 blockers, logged in report & tests.txt
                red_text = (
                    "ORCHESTRATOR SUMMARY: exit=101 santa_blocks=0 passed=20 failed=2 scoped=true executed=yes (scoped)\n\n"
                    "test mining_advanced::test_mn15_corp_mining_ledger_service_and_observer_tracking ... FAILED\n"
                    "test dispatch_unit_tests::dispatch_rpc_p0_1350_preauth_gm_role_and_moniker_gates ... FAILED\n"
                    "test result: FAILED. 20 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out\n"
                )
                tm_red = {"exit": 101, "santa_blocks": 0, "passed": 20, "failed": 2, "killed": False, "scoped": True, "non_test_failure": False}
                so1 = {"verdict": "PASS", "summary": "Looks solid.", "blocking_findings": [], "report_markdown": "Review body."}
                v1, text1, tm1 = run_review._evaluate_post_review_test_gate(
                    repo, c1, feat_head, ctx, red_text, tm_red, "PASS", so1
                )
                self.assertEqual(v1, "PASS")
                self.assertEqual(so1["blocking_findings"], [])
                self.assertTrue(tm1.get("red_main_subtracted"))
                self.assertIn("Pre-existing red tests on main (#1587)", text1)
                self.assertIn("Pre-existing red tests on main (#1587)", so1["report_markdown"])
                rendered = run_review.render_report({"verdict": v1, "mode": "claims"}, so1)
                self.assertIn("Pre-existing red tests on main (#1587)", rendered)
                self.assertIn("mining_advanced::test_mn15_corp_mining_ledger_service_and_observer_tracking", rendered)

                # 2. Positive when branch cut at c0 (before c1) while c1 is in origin/main lineage before any fix commit
                _git(repo, "checkout", "-b", "feat/pre-c1", c0)
                _write(os.path.join(repo, "pre.txt"), "pre-c1 work\n")
                _git(repo, "add", ".")
                _git(repo, "commit", "-m", "feat: cut before c1")
                pre_head = _git(repo, "rev-parse", "HEAD")
                so_pre = {"verdict": "PASS_WITH_ISSUES", "summary": "Minor nits.", "blocking_findings": [], "report_markdown": "Body."}
                v_pre, _, _ = run_review._evaluate_post_review_test_gate(
                    repo, c0, pre_head, ctx, red_text, tm_red, "PASS_WITH_ISSUES", so_pre
                )
                self.assertEqual(v_pre, "PASS_WITH_ISSUES")
                self.assertEqual(so_pre["blocking_findings"], [])

                # 3. Negative (mixed pre-existing + new failure): MUST fail hard
                mixed_text = (
                    "ORCHESTRATOR SUMMARY: exit=101 santa_blocks=0 passed=19 failed=2 scoped=true executed=yes (scoped)\n\n"
                    "test mining_advanced::test_mn15_corp_mining_ledger_service_and_observer_tracking ... FAILED\n"
                    "test my_new_module::test_brand_new_regression ... FAILED\n"
                    "test result: FAILED. 19 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out\n"
                )
                so_mix = {"verdict": "PASS", "summary": "ok", "blocking_findings": [], "report_markdown": "Body."}
                v_mix, _, tm_mix = run_review._evaluate_post_review_test_gate(
                    repo, c1, feat_head, ctx, mixed_text, tm_red, "PASS", so_mix
                )
                self.assertEqual(v_mix, "FAIL")
                self.assertEqual(so_mix["blocking_findings"][0]["id"], "ORCHESTRATOR-POST-REVIEW-TEST-FAIL")
                self.assertEqual(tm_mix.get("new_failures"), ["my_new_module::test_brand_new_regression"])

                # 4. Negative (bare test name or partial non-:: prefix collision must NOT match)
                bare_text = (
                    "test test_mn15_corp_mining_ledger_service_and_observer_tracking ... FAILED\n"
                    "test x_mining_advanced::test_mn15_corp_mining_ledger_service_and_observer_tracking ... FAILED\n"
                    "test result: FAILED. 10 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out\n"
                )
                so_bare = {"verdict": "PASS", "summary": "ok", "blocking_findings": [], "report_markdown": "Body."}
                v_bare, _, _ = run_review._evaluate_post_review_test_gate(
                    repo, c1, feat_head, ctx, bare_text, tm_red, "PASS", so_bare
                )
                self.assertEqual(v_bare, "FAIL")

                # 5. Negative (branch claims Fixes #1587 or Closes #1587): subtraction MUST be bypassed!
                for kw in ("Fixes #1587", "Closes #1587"):
                    bname = "hotfix/1587-" + kw.split()[0].lower()
                    _git(repo, "checkout", "-b", bname, c1)
                    _write(os.path.join(repo, bname.replace("/", "_") + ".txt"), "attempt\n")
                    _git(repo, "add", ".")
                    _git(repo, "commit", "-m", "fix(ci): attempt corp fix (%s)" % kw)
                    fix_head = _git(repo, "rev-parse", "HEAD")
                    so_fix = {"verdict": "PASS", "summary": "ok", "blocking_findings": [], "report_markdown": "Body."}
                    v_fix, _, _ = run_review._evaluate_post_review_test_gate(
                        repo, c1, fix_head, ctx, red_text, tm_red, "PASS", so_fix
                    )
                    self.assertEqual(v_fix, "FAIL", "Branch claiming %s must NOT subtract #1587 failures" % kw)
                    self.assertEqual(so_fix["blocking_findings"][0]["id"], "ORCHESTRATOR-POST-REVIEW-TEST-FAIL")

                # 6. Negative (non_test_failure / compile error or final_run=True or unclosed_rows): MUST fail hard
                so_ntf = {"verdict": "PASS", "summary": "ok", "blocking_findings": [], "report_markdown": "Body."}
                v_ntf, _, _ = run_review._evaluate_post_review_test_gate(
                    repo, c1, feat_head, ctx, red_text, dict(tm_red, non_test_failure=True), "PASS", so_ntf
                )
                self.assertEqual(v_ntf, "FAIL")
                so_fin = {"verdict": "PASS", "summary": "ok", "blocking_findings": [], "report_markdown": "Body."}
                v_fin, _, _ = run_review._evaluate_post_review_test_gate(
                    repo, c1, feat_head, ctx, red_text, tm_red, "PASS", so_fin, final_run=True
                )
                self.assertEqual(v_fin, "FAIL")

                # 7. Negative (status != FAIL, missing p0_issue.number, empty failed_tests)
                for bad_patch in ({"status": "PASS"}, {"p0_issue": {}}, {"failed_tests": []}):
                    bad_doc = dict(base_state, **bad_patch)
                    _write(state_path, json.dumps(bad_doc))
                    so_bad = {"verdict": "PASS", "summary": "ok", "blocking_findings": [], "report_markdown": "Body."}
                    v_bad, _, _ = run_review._evaluate_post_review_test_gate(
                        repo, c1, feat_head, ctx, red_text, tm_red, "PASS", so_bad
                    )
                    self.assertEqual(v_bad, "FAIL", "Malformed/non-FAIL state %r must not subtract" % (bad_patch,))

                # 8. Negative (origin/main already moved past a fix commit closing #1587 while state JSON is stale)
                _write(state_path, json.dumps(base_state))
                _git(repo, "checkout", "main")
                _write(os.path.join(repo, "a.txt"), "fixed-on-main\n")
                _git(repo, "add", ".")
                _git(repo, "commit", "-m", "fix(corp): restore role check (Fixes #1587)")
                c2_fixed = _git(repo, "rev-parse", "HEAD")
                _git(repo, "update-ref", "refs/remotes/origin/main", c2_fixed)
                so_stale = {"verdict": "PASS", "summary": "ok", "blocking_findings": [], "report_markdown": "Body."}
                v_stale, _, _ = run_review._evaluate_post_review_test_gate(
                    repo, c1, feat_head, ctx, red_text, tm_red, "PASS", so_stale
                )
                self.assertEqual(v_stale, "FAIL")
            finally:
                if old_env is None:
                    os.environ.pop("EVOMU_HOURLY_CI_STATE_PATH", None)
                else:
                    os.environ["EVOMU_HOURLY_CI_STATE_PATH"] = old_env

    def test_1587_nested_tests_it_submodules_included_in_it_tokens_and_head_it(self):
        from unittest import mock

        # 1. Live repo index must discover nested `tests/it/*/*.rs` submodules recursively
        idx = run_review._load_or_build_reverse_dep_index(REPO_ROOT)
        self.assertIn("operation_replay_and_container_password_notices", idx["it_tokens"])
        self.assertIn("esi_universe_sov_ext_2", idx["it_tokens"])

        # 2. Modifying a nested IT submodule must NOT be silently dropped for either head="HEAD" or explicit commit SHA
        nested_files = [
            "crates/eve-server/tests/it/notify_events/operation_replay_and_container_password_notices.rs",
            "crates/eve-server/tests/it/esi_universe_sov_complete/esi_universe_sov_ext_2.rs",
        ]
        head_sha = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True, check=True
        ).stdout.strip()
        for head_ref in ("HEAD", head_sha):
            def _fake_sh(cmd, cwd, env=None, timeout=None):
                if cmd[:3] == ["git", "diff", "--name-only"]:
                    return 0, "\n".join(nested_files) + "\n"
                if cmd[:3] == ["git", "diff", "-U0"]:
                    return 0, ""
                return 0, ""

            with mock.patch.object(run_review, "sh", side_effect=_fake_sh):
                cmds = run_review.derive_affected_test_commands(REPO_ROOT, "HEAD~1", "cargo", head=head_ref)
            it_filters = [c[-1] for c in cmds if "--test" in c and "it" in c]
            self.assertIn(
                "operation_replay_and_container_password_notices",
                it_filters,
                "nested notify_events IT submodule dropped for head=%s: %s" % (head_ref, cmds),
            )
            self.assertIn(
                "esi_universe_sov_ext_2",
                it_filters,
                "nested esi_universe_sov_complete IT submodule dropped for head=%s: %s" % (head_ref, cmds),
            )

    def test_arch_check_37_claims_head_sha_fast_fail_in_preflight_and_run_review(self):
        import review_lib as rl  # noqa: E402

        head_sha = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True, check=True
        ).stdout.strip()
        short_sha = head_sha[:9]
        parent_sha = subprocess.run(
            ["git", "rev-parse", "HEAD~1"], cwd=REPO_ROOT, capture_output=True, text=True, check=True
        ).stdout.strip()
        stale_sha = "deadbeef1234567890abcdef1234567890abcdef"
        if stale_sha.startswith(head_sha[:7]):
            stale_sha = "cafebabe1234567890abcdef1234567890abcdef"

        with tempfile.TemporaryDirectory() as tmp:
            # 1. All real matching HEAD header formats pass (short, full, case-insensitive)
            matching_headers = [
                "HEAD: %s\n" % short_sha.upper(),
                "- **Branch HEAD:** `%s`\n" % head_sha,
                "- **Reviewed HEAD:** `%s`\n" % short_sha,
                "- **Head Commit:** `%s` (rebased onto `origin/main` `%s`)\n" % (head_sha, parent_sha[:9]),
                "- **Branch / Commit:** `arch/931-t3b-switch` @ `%s`\n" % head_sha,
                "- **Branch / HEAD:** `arch/1194-t1b-corp-actor` at `%s` (`0` commits behind `origin/main` `%s`).\n" % (head_sha, parent_sha[:9]),
                "- **Branch / Base / Commit:** `feat/x` at commit `%s`, cleanly rebased onto `origin/main` `%s`\n" % (head_sha, parent_sha),
                "- `%s` (`HEAD`): `fix: something`\n" % head_sha,
                "Commit `%s` (`%s`, based on `%s`) fixes the thing.\n" % (head_sha, short_sha, parent_sha[:9]),
            ]
            for idx, body in enumerate(matching_headers):
                p = os.path.join(tmp, "match_%d.md" % idx)
                _write(p, body)
                self.assertEqual(
                    rl.check_claims_head_sha(REPO_ROOT, p, target_commit="HEAD"),
                    [],
                    "Expected matching HEAD header #%d to pass: %r" % (idx, body),
                )

            # 2. Base / origin/main / historical body SHAs & no-HEAD claims MUST NOT false-positive
            no_head_cases = [
                "# Claims without any commit SHA\n- Changed foo and bar.\n",
                "- **Base:** `origin/main` (`%s`)\n"
                "- **Base commit (`origin/main`):** `%s`\n"
                "- **Base SHA (`origin/main` pre-rebase):** `%s`\n"
                "- **Merge-Base / Trunk:** Rebased onto `%s`; current `origin/main` is at `%s`.\n"
                "- **Branch / Base:** `arch/1540-t4b` (branched from `%s`)\n"
                "- **Branch:** `feat/x` (`origin/main` merged cleanly at `%s`)\n"
                "- **Status:** `NO_CARGO_READY`\n"
                "## Root Cause\n"
                "- Commit `%s` updated legacy logic.\n"
                "```\nHEAD: %s\n```\n"
                % (
                    stale_sha[:9],
                    stale_sha,
                    stale_sha,
                    stale_sha,
                    stale_sha,
                    stale_sha[:9],
                    stale_sha,
                    stale_sha[:9],
                    stale_sha,
                ),
            ]
            for idx, body in enumerate(no_head_cases):
                p = os.path.join(tmp, "no_head_%d.md" % idx)
                _write(p, body)
                self.assertEqual(
                    rl.extract_declared_claims_head_shas(body),
                    [],
                    "False-positive HEAD SHA extracted from non-HEAD case #%d" % idx,
                )
                self.assertEqual(rl.check_claims_head_sha(REPO_ROOT, p, target_commit="HEAD"), [])

            # 3. Mismatched HEAD SHA (like #931 3B-1 stale pre-rebase HEAD) fails fast in library, preflight, and run_review
            stale_claims = os.path.join(tmp, "stale_claims.md")
            _write(
                stale_claims,
                "# Claims\n"
                "- **Branch / Commit:** `arch/931-t3b-switch` @ `%s`\n"
                "- **Merge-Base / Trunk:** Rebased onto `%s`\n"
                % (stale_sha, parent_sha),
            )
            viols = rl.check_claims_head_sha(REPO_ROOT, stale_claims, target_commit="HEAD")
            self.assertEqual(len(viols), 1)
            self.assertEqual(viols[0][0], "CLAIMS_STALE_HEAD_SHA")
            self.assertIn(stale_sha, viols[0][1])
            self.assertIn(head_sha[:12], viols[0][1])

            # 3a. preflight.py --early-only --claims <stale> fails fast non-zero in < 2s (warm < 1s)
            preflight_py = os.path.join(SCRIPTS_DIR, "preflight.py")
            pf_proc = subprocess.run(
                [sys.executable, preflight_py, "--early-only", "--claims", stale_claims],
                cwd=REPO_ROOT,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(pf_proc.returncode, 0, pf_proc.stdout + pf_proc.stderr)
            self.assertIn("CLAIMS_STALE_HEAD_SHA", pf_proc.stdout + pf_proc.stderr)
            self.assertIn(stale_sha, pf_proc.stdout + pf_proc.stderr)
            self.assertIn(head_sha[:12], pf_proc.stdout + pf_proc.stderr)

            # 3b. run_review.py (both default snapshot mode and --in-place) fails fast in < 1s BEFORE snapshot/cargo/LLM
            run_review_py = os.path.join(SCRIPTS_DIR, "run_review.py")
            for extra_args in ([], ["--in-place"]):
                t0 = time.monotonic()
                rr_proc = subprocess.run(
                    [sys.executable, run_review_py, "--claims-file", stale_claims] + extra_args,
                    cwd=REPO_ROOT,
                    capture_output=True,
                    text=True,
                )
                elapsed = time.monotonic() - t0
                self.assertEqual(rr_proc.returncode, run_review.EXIT["FAIL"], rr_proc.stdout + rr_proc.stderr)
                self.assertIn("CLAIMS_STALE_HEAD_SHA", rr_proc.stdout + rr_proc.stderr)
                self.assertIn(stale_sha, rr_proc.stdout + rr_proc.stderr)
                self.assertLess(elapsed, 1.0, "run_review.py fast-fail took %.3fs (>= 1.0s)" % elapsed)

            # 4. Target commit override (`--commit HEAD~1`) checks against parent_sha, not HEAD
            parent_claims = os.path.join(tmp, "parent_claims.md")
            _write(parent_claims, "Reviewed HEAD: `%s`\n" % parent_sha[:9])
            self.assertEqual(rl.check_claims_head_sha(REPO_ROOT, parent_claims, target_commit=parent_sha), [])
            self.assertEqual(len(rl.check_claims_head_sha(REPO_ROOT, parent_claims, target_commit="HEAD")), 1)

    def test_arch_check_39_behind_trunk_ratchet_fast_fail(self):
        import review_lib as rl  # noqa: E402

        def _git(cwd, *args):
            p = subprocess.run(["git"] + list(args), cwd=cwd, capture_output=True, text=True, check=True)
            return p.stdout.strip()

        # Verify ratchet JSON path classifier directly
        self.assertTrue(rl.is_ratchet_baseline_json("crates/eve-testkit/ratchets/arch_debt_ratchet.json"))
        self.assertTrue(rl.is_ratchet_baseline_json("crates/eve-proto/ratchets/raw_encode_field_ratchet.json"))
        self.assertTrue(
            rl.is_ratchet_baseline_json(".agents/skills/evomu-adversarial-review/references/stringly_error_bypass_ratchet.json")
        )
        self.assertFalse(rl.is_ratchet_baseline_json("crates/eve-testkit/src/arch_debt_ratchet.rs"))
        self.assertFalse(rl.is_ratchet_baseline_json("crates/eve-server/src/services/foo.rs"))
        self.assertFalse(rl.is_ratchet_baseline_json(".agents/skills/evomu-adversarial-review/references/parity/ledger.json"))
        self.assertFalse(rl.is_ratchet_baseline_json("README.md"))

        preflight_py = os.path.join(SCRIPTS_DIR, "preflight.py")
        run_review_py = os.path.join(SCRIPTS_DIR, "run_review.py")

        with tempfile.TemporaryDirectory() as tmp:
            repo = os.path.join(tmp, "repo")
            os.makedirs(repo)
            _git(repo, "init", "-b", "main")
            _git(repo, "config", "user.email", "test@example.com")
            _git(repo, "config", "user.name", "Test")
            _write(os.path.join(repo, "README.md"), "base\n")
            _write(os.path.join(repo, "crates/eve-server/tests/it/rpc_client_contracts.rs"), "// stub\n")
            _write(os.path.join(repo, "crates/eve-server/tests/it/main.rs"), "mod rpc_client_contracts;\n")
            _write(os.path.join(repo, "crates/eve-testkit/ratchets/arch_debt_ratchet.json"), '{"version": 2, "patterns": {}}\n')
            _write(
                os.path.join(repo, ".agents/skills/evomu-adversarial-review/references/stringly_error_bypass_ratchet.json"),
                json.dumps({
                    "version": 2,
                    "rules": {
                        "const_err_str_defs": {"files": {}},
                        "bridge_rpc_error_nonlit_code": {"files": {}},
                        "bridge_rpc_error_helper_defs": {"files": {}},
                        "bridge_rpc_error_helper_calls": {"files": {}},
                    },
                }) + "\n",
            )
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "initial")
            c0 = _git(repo, "rev-parse", "HEAD")
            _git(repo, "update-ref", "refs/remotes/origin/main", c0)

            # Create feature branch off c0
            _git(repo, "checkout", "-b", "feat/work", c0)
            _write(os.path.join(repo, "notes.md"), "feature work\n")
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "feat: non-ratchet work")
            feat_sha = _git(repo, "rev-parse", "HEAD")

            # 1. Up-to-date branch -> no violation
            self.assertEqual(rl.check_behind_trunk_ratchet(repo, target_commit="HEAD"), [])

            # 2. Trunk advances with a NON-ratchet commit -> MUST NOT trigger BEHIND_TRUNK_RATCHET
            _git(repo, "checkout", "main")
            _write(os.path.join(repo, "README.md"), "trunk non-ratchet advance\n")
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "docs: update readme on trunk")
            c_non_ratchet = _git(repo, "rev-parse", "HEAD")
            _git(repo, "update-ref", "refs/remotes/origin/main", c_non_ratchet)

            _git(repo, "checkout", "feat/work")
            self.assertEqual(rl.check_behind_trunk_ratchet(repo, target_commit="HEAD"), [])
            pf_ok = subprocess.run(
                [sys.executable, preflight_py, "--early-only"],
                cwd=repo,
                capture_output=True,
                text=True,
            )
            self.assertEqual(pf_ok.returncode, 0, pf_ok.stdout + pf_ok.stderr)
            self.assertNotIn("BEHIND_TRUNK_RATCHET", pf_ok.stdout + pf_ok.stderr)

            # 3. Trunk advances with a `crates/eve-testkit/ratchets/*.json` update -> fast-fail everywhere!
            _git(repo, "checkout", "main")
            _write(
                os.path.join(repo, "crates/eve-testkit/ratchets/arch_debt_ratchet.json"),
                '{"version": 2, "patterns": {"demo": {"files": {}}}}\n',
            )
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "chore: tighten arch_debt_ratchet")
            c_ratchet1 = _git(repo, "rev-parse", "HEAD")
            _git(repo, "update-ref", "refs/remotes/origin/main", c_ratchet1)

            _git(repo, "checkout", "feat/work")
            viols1 = rl.check_behind_trunk_ratchet(repo, target_commit="HEAD")
            self.assertEqual(len(viols1), 1)
            self.assertEqual(viols1[0][0], "BEHIND_TRUNK_RATCHET")
            self.assertIn("crates/eve-testkit/ratchets/arch_debt_ratchet.json", viols1[0][1])
            self.assertIn("Rebase onto origin/main", viols1[0][1])

            # 3a. preflight.py --early-only fails fast with BEHIND_TRUNK_RATCHET
            pf_fail = subprocess.run(
                [sys.executable, preflight_py, "--early-only"],
                cwd=repo,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(pf_fail.returncode, 0, pf_fail.stdout + pf_fail.stderr)
            self.assertIn("BEHIND_TRUNK_RATCHET", pf_fail.stdout + pf_fail.stderr)
            self.assertIn("crates/eve-testkit/ratchets/arch_debt_ratchet.json", pf_fail.stdout + pf_fail.stderr)

            # 3b. run_review.py (both default snapshot mode and --in-place) fails fast in < 1s BEFORE worktree/cargo/LLM
            for extra_args in ([], ["--in-place"], ["--commit", feat_sha]):
                t0 = time.monotonic()
                rr_fail = subprocess.run(
                    [sys.executable, run_review_py] + extra_args,
                    cwd=repo,
                    capture_output=True,
                    text=True,
                )
                elapsed = time.monotonic() - t0
                self.assertEqual(rr_fail.returncode, run_review.EXIT["FAIL"], rr_fail.stdout + rr_fail.stderr)
                self.assertIn("BEHIND_TRUNK_RATCHET", rr_fail.stdout + rr_fail.stderr)
                self.assertIn("crates/eve-testkit/ratchets/arch_debt_ratchet.json", rr_fail.stdout + rr_fail.stderr)
                self.assertLess(elapsed, 1.0, "run_review.py BEHIND_TRUNK_RATCHET fast-fail took %.3fs" % elapsed)

            # 4. Also catches `.agents/skills/evomu-adversarial-review/references/*ratchet*.json` updates on trunk
            _git(repo, "checkout", "main")
            _write(
                os.path.join(repo, ".agents/skills/evomu-adversarial-review/references/stringly_error_bypass_ratchet.json"),
                '{"version": 2, "rules": {"tightened": true}}\n',
            )
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "chore: tighten stringly_error_bypass_ratchet")
            c_ratchet2 = _git(repo, "rev-parse", "HEAD")
            _git(repo, "update-ref", "refs/remotes/origin/main", c_ratchet2)

            _git(repo, "checkout", "feat/work")
            viols2 = rl.check_behind_trunk_ratchet(repo, target_commit="HEAD")
            self.assertEqual(len(viols2), 1)
            self.assertIn("crates/eve-testkit/ratchets/arch_debt_ratchet.json", viols2[0][1])
            self.assertIn(
                ".agents/skills/evomu-adversarial-review/references/stringly_error_bypass_ratchet.json",
                viols2[0][1],
            )

            # 5. Rebased branch (or trunk itself) -> no violation!
            _git(repo, "rebase", "origin/main")
            self.assertEqual(rl.check_behind_trunk_ratchet(repo, target_commit="HEAD"), [])
            self.assertEqual(rl.check_behind_trunk_ratchet(repo, target_commit=c_ratchet2), [])

    def test_arch_check_39_does_not_compile_precheck_and_run_tests_early_abort(self):
        def _git(cwd, *args):
            p = subprocess.run(["git"] + list(args), cwd=cwd, capture_output=True, text=True, check=True)
            return p.stdout.strip()

        # 1. Diff classifier & compile-error detector unit checks
        self.assertTrue(run_review._diff_touches_rust(["crates/eve-server/src/services/foo.rs"]))
        self.assertTrue(run_review._diff_touches_rust(["Cargo.toml"]))
        self.assertTrue(run_review._diff_touches_rust(["crates/eve-server/Cargo.toml"]))
        self.assertTrue(run_review._diff_touches_rust(["Cargo.lock"]))
        self.assertFalse(run_review._diff_touches_rust(["tools/client-harness/playtester.py", "docs/notes.md"]))
        self.assertFalse(run_review._diff_touches_rust(["crates/eve-testkit/ratchets/arch_debt_ratchet.json"]))
        self.assertFalse(run_review._diff_touches_rust([]))

        compile_err_out = (
            "   Compiling eve-server v0.1.0\n"
            "error[E0425]: cannot find value `missing_var` in this scope\n"
            "  --> crates/eve-server/src/services/foo.rs:42:13\n"
            "error: could not compile `eve-server` (lib) due to 1 previous error\n"
        )
        infra_err_out = (
            "error: couldn't create a temp dir: No such file or directory (os error 2) "
            "at /tmp/review/target/debug/deps/rustc123\n"
        )
        self.assertTrue(run_review._is_cargo_compile_error(compile_err_out, 101, failed=0))
        self.assertFalse(run_review._is_cargo_compile_error(compile_err_out, 0, failed=0))
        self.assertFalse(run_review._is_cargo_compile_error(compile_err_out, 101, failed=1))
        self.assertFalse(run_review._is_cargo_compile_error(infra_err_out, 101, failed=0))

        # 2. `run_tests` defense-in-depth: aborts remaining cargo targets immediately on compile error
        multi_cmds = [
            ["cargo", "check", "-j", "4", "--workspace", "--bins"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", "rpc_client_contracts"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "macho_listener"],
            ["cargo", "test", "-j", "4", "-p", "eve-proto", "--lib"],
        ]
        seen_compile = []

        def fake_compile_fail(cmd, cwd, env=None, timeout=None):
            seen_compile.append(list(cmd))
            return 101, compile_err_out

        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.object(run_review, "sh", side_effect=fake_compile_fail), mock.patch.object(
                run_review, "_acquire_cargo_test_slot", return_value=None
            ), mock.patch.object(run_review, "derive_affected_test_commands", return_value=multi_cmds[1:]):
                txt, tm = run_review.run_tests(REPO_ROOT, tmp, timeout=30, base="HEAD")
            self.assertEqual(len(seen_compile), 1, "run_tests must abort after first compile error, not retry all targets")
            self.assertTrue(tm.get("compile_error"))
            self.assertIn("DOES_NOT_COMPILE", txt)
            so_eval = {"verdict": "PASS", "summary": "ok", "blocking_findings": [], "report_markdown": "Body."}
            v_eval, _, _ = run_review._evaluate_post_review_test_gate(
                REPO_ROOT, "HEAD", "HEAD", tmp, txt, tm, "PASS", so_eval
            )
            self.assertEqual(v_eval, "FAIL")
            self.assertIn("DOES_NOT_COMPILE", so_eval["blocking_findings"][0]["title"])

            # When compile_prechecked=True, run_tests skips the redundant `cargo check --workspace --bins`
            seen_prechecked = []

            def fake_pass(cmd, cwd, env=None, timeout=None):
                seen_prechecked.append(list(cmd))
                return 0, "test demo ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured\n"

            with mock.patch.object(run_review, "sh", side_effect=fake_pass), mock.patch.object(
                run_review, "_acquire_cargo_test_slot", return_value=None
            ), mock.patch.object(run_review, "derive_affected_test_commands", return_value=multi_cmds[1:]):
                _, tm_ok = run_review.run_tests(REPO_ROOT, tmp, timeout=30, base="HEAD", compile_prechecked=True)
            self.assertEqual(tm_ok["exit"], 0)
            self.assertFalse(any(c[1] == "check" for c in seen_prechecked), seen_prechecked)

        # 3. End-to-end `run_review.main()` Step 0 compile gate tests (0 real cargo, 0 real LLM)
        with tempfile.TemporaryDirectory() as tmp:
            repo = os.path.join(tmp, "repo")
            os.makedirs(repo)
            _git(repo, "init", "-b", "main")
            _git(repo, "config", "user.email", "test@example.com")
            _git(repo, "config", "user.name", "Test")
            _write(os.path.join(repo, "crates/eve-server/src/lib.rs"), "pub fn base() {}\n")
            _write(os.path.join(repo, "tools/client-harness/playtester.py"), "X = 1\n")
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "base")
            c0 = _git(repo, "rev-parse", "HEAD")
            _git(repo, "update-ref", "refs/remotes/origin/main", c0)

            # Branch A: Rust-touching commit
            _git(repo, "checkout", "-b", "feat/rust-change", c0)
            _write(os.path.join(repo, "crates/eve-server/src/lib.rs"), "pub fn broken() { missing_var }\n")
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "feat: rust change")

            # Branch B: Pure-Python commit
            _git(repo, "checkout", "-b", "feat/py-only", c0)
            _write(os.path.join(repo, "tools/client-harness/playtester.py"), "X = 2\n")
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "fix: python only change")

            def _run_main_in(branch, cargo_side_effect, extra_argv=()):
                _git(repo, "checkout", branch)
                cargo_cmds = []
                real_sh = run_review.sh

                def _sh_router(cmd, cwd, env=None, timeout=None):
                    if cmd and os.path.basename(cmd[0]) == "cargo":
                        cargo_cmds.append((list(cmd), dict(env or {})))
                        return cargo_side_effect(cmd, cwd, env=env, timeout=timeout)
                    if cmd and cmd[0] == sys.executable and any("metrics" in str(x) for x in cmd):
                        return 0, "(mocked metrics)\n"
                    return real_sh(cmd, cwd, env=env, timeout=timeout)

                fake_pass_so = {
                    "verdict": "PASS",
                    "summary": "Clean review.",
                    "blocking_findings": [],
                    "claims_assessment": [],
                    "open_findings_status": [],
                    "report_markdown": "Clean.",
                }
                fake_test_run = mock.Mock()
                fake_test_run.join.return_value = (
                    "ORCHESTRATOR SUMMARY: exit=0 santa_blocks=0 passed=5 failed=0 scoped=true executed=yes (scoped)\n"
                    "test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured\n",
                    {"exit": 0, "santa_blocks": 0, "passed": 5, "failed": 0, "killed": False, "scoped": True},
                    0.1,
                )

                def _fake_run_parity(root_p, sdir_p, ctx_p, assume_tests_pass=False, out_name="parity.json", md_name="parity.md"):
                    summ = {"percent_closed": 100.0, "closed_rows": 1, "total_rows": 1, "complete": True}
                    _write(os.path.join(ctx_p, out_name), json.dumps({"summary": summ, "rows": {}}))
                    _write(os.path.join(ctx_p, md_name), "# Parity\n")
                    return summ, "# Parity\n"

                old_cwd = os.getcwd()
                os.chdir(repo)
                try:
                    with mock.patch.object(sys, "argv", ["run_review.py", "--in-place", "--no-auto-audit"] + list(extra_argv)), \
                            mock.patch.object(run_review, "sh", side_effect=_sh_router), \
                            mock.patch.object(run_review, "_acquire_cargo_test_slot", return_value=None) as mock_slot, \
                            mock.patch.object(run_review, "run_parity", side_effect=_fake_run_parity), \
                            mock.patch.object(run_review, "launch_reviewer", return_value=("PASS", dict(fake_pass_so), "conv-1", "mock-model")) as mock_llm, \
                            mock.patch.object(run_review, "TestRun", return_value=fake_test_run) as mock_tr:
                        rc = run_review.main()
                    return rc, cargo_cmds, mock_llm, mock_tr, mock_slot
                finally:
                    os.chdir(old_cwd)

            # 3a. Rust diff + compile error -> aborts with FAIL / DOES_NOT_COMPILE BEFORE launching LLM or TestRun!
            rc_fail, cmds_fail, llm_fail, tr_fail, slot_fail = _run_main_in(
                "feat/rust-change",
                lambda cmd, cwd, env=None, timeout=None: (101, compile_err_out),
            )
            self.assertEqual(rc_fail, run_review.EXIT["FAIL"])
            llm_fail.assert_not_called()
            tr_fail.assert_not_called()
            slot_fail.assert_called_once()
            self.assertEqual(len(cmds_fail), 1)
            self.assertEqual(cmds_fail[0][0][1:], ["check", "-j", "4", "--workspace", "--tests"])
            self.assertEqual(cmds_fail[0][1].get("CARGO_INCREMENTAL"), "0")
            self.assertEqual(cmds_fail[0][1].get("CARGO_BUILD_JOBS"), "4")
            latest_md = open(os.path.join(repo, ".evomu-reviews", "latest.md"), encoding="utf-8").read()
            self.assertIn("DOES_NOT_COMPILE", latest_md)
            self.assertIn("error[E0425]", latest_md)

            # 3b. Pure-Python diff -> Step 0 compile precheck SKIPPED completely (0 cargo calls!) and LLM runs!
            rc_py, cmds_py, llm_py, tr_py, slot_py = _run_main_in(
                "feat/py-only",
                lambda cmd, cwd, env=None, timeout=None: (_ for _ in ()).throw(AssertionError("cargo must not run for pure Python diff")),
            )
            self.assertEqual(rc_py, run_review.EXIT["PASS"])
            self.assertEqual(cmds_py, [])
            slot_py.assert_not_called()
            llm_py.assert_called_once()
            tr_py.assert_called_once()
            self.assertFalse(tr_py.call_args.kwargs.get("compile_prechecked"))

            # 3c. Rust diff + `--skip-tests` -> Step 0 compile precheck SKIPPED (0 cargo calls!) and LLM runs!
            rc_skip, cmds_skip, llm_skip, tr_skip, slot_skip = _run_main_in(
                "feat/rust-change",
                lambda cmd, cwd, env=None, timeout=None: (_ for _ in ()).throw(AssertionError("cargo must not run with --skip-tests")),
                extra_argv=["--skip-tests"],
            )
            self.assertEqual(rc_skip, run_review.EXIT["PASS"])
            self.assertEqual(cmds_skip, [])
            slot_skip.assert_not_called()
            llm_skip.assert_called_once()
            tr_skip.assert_not_called()

            # 3d. Rust diff + Step 0 compile precheck passes -> LLM runs AND TestRun receives compile_prechecked=True!
            rc_ok, cmds_ok, llm_ok, tr_ok, _ = _run_main_in(
                "feat/rust-change",
                lambda cmd, cwd, env=None, timeout=None: (0, "    Finished dev profile\n"),
            )
            self.assertEqual(rc_ok, run_review.EXIT["PASS"])
            self.assertEqual(len(cmds_ok), 1)
            self.assertEqual(cmds_ok[0][0][1:], ["check", "-j", "4", "--workspace", "--tests"])
            llm_ok.assert_called_once()
            tr_ok.assert_called_once()
            self.assertTrue(tr_ok.call_args.kwargs.get("compile_prechecked"))

            # 3e. Rust diff + persistent target/ `os error 2` infra race in Step 0 -> retried once, recorded as ERROR (3), not FAIL!
            rc_infra, cmds_infra, llm_infra, tr_infra, _ = _run_main_in(
                "feat/rust-change",
                lambda cmd, cwd, env=None, timeout=None: (101, infra_err_out),
            )
            self.assertEqual(rc_infra, 3)
            self.assertEqual(len(cmds_infra), 2, "Step 0 infra race must retry once")
            llm_infra.assert_not_called()
            tr_infra.assert_not_called()
            infra_md = open(os.path.join(repo, ".evomu-reviews", "latest.md"), encoding="utf-8").read()
            self.assertIn("| verdict | `ERROR` |", infra_md)

    def test_1583_process_global_ratchet_parity_enforcement_and_tightening(self):
        import tighten_ratchets  # noqa: E402

        def _empty_ratchet_doc():
            return {
                "version": 1,
                "owning_issue": 1583,
                "description": "test process_global ratchet",
                "excluded_crates": ["eve-testkit"],
                "groups": {
                    g: {
                        "zero_required": False,
                        "max_count": 10,
                        "rationale_required": (g == "G_process_wide_allowlist"),
                        "description": "group %s" % g,
                    }
                    for g in ratchet_checks.PROCESS_GLOBAL_GROUPS
                },
                "declarations": [],
            }

        # 1. Scanner parity: comments, strings, provably immutable statics, #[cfg(test)] depth-aware items,
        #    and IMMUTABLE_STATIC_TABLES allowlist vs static_mut, static_sync, thread_local!, tokio::task_local!
        sample_src = (
            '// static COMMENTED_OUT: OnceLock<u64> = OnceLock::new();\n'
            '/*\n'
            '    static BLOCK_COMMENT: Mutex<u64> = Mutex::new(0);\n'
            '    tokio::task_local! { static IN_BLOCK_COMMENT: u64; }\n'
            '*/\n'
            'const RAW: &str = r#"thread_local! { static IN_STRING: Cell<u64> = const { Cell::new(0) }; }"#;\n'
            'static CONST_SCALAR: u64 = 42;\n'
            'static CONST_TABLE: &[u32] = &[1, 2, 3];\n'
            'static CONST_BYTES: [u8; 4] = [1, 2, 3, 4];\n'
            'static CONST_SHIFT_BYTES: [u8; 1 << 4] = [0; 16];\n'
            'static CONST_SHIFT_TUPLE: ([u8; 1 << 4], usize) = ([0; 16], 16);\n'
            'static CONST_PAIRS: &[(&\'static str, u16)] = &[("a", 1)];\n'
            'static LIFE_USER: &\'static str = "ok";\n'
            'static MUT_COUNTER: AtomicU64 = AtomicU64::new(0);\n'
            'static MUT_GLOBAL_CELL: OnceLock<Mutex<u64>> = OnceLock::new();\n'
            'static ONCE_FLAG: std::sync::Once = std::sync::Once::new();\n'
            'static mut RAW_MUT: u32 = 0;\n'
            'static WRAPPER_GLOBAL: GlobalCounter = GlobalCounter::new();\n'
            'static MUTABLE_ARRAY_ELEM: [std::sync::atomic::AtomicU64; 2] = [];\n'
            'static MUTABLE_TUPLE_ELEM: (u32, std::sync::Mutex<u32>) = (1, std::sync::Mutex::new(2));\n'
            'thread_local! {\n'
            '    static TL_ONE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };\n'
            '    static TL_TWO: std::cell::RefCell<Option<u64>> = const { std::cell::RefCell::new(None) };\n'
            '}\n'
            'tokio::task_local! {\n'
            '    static TASK_SCOPE: bool;\n'
            '}\n'
            'struct Config {\n'
            '    #[cfg(test)]\n'
            '    pub test_map: std::collections::HashMap<i64, u64>,\n'
            '    #[cfg(test)]\n'
            '    pub tail_without_comma: [u8; 4]\n'
            '}\n'
            'static AFTER_STRUCT: OnceLock<u64> = OnceLock::new();\n'
            'fn dispatch(code: u32) -> u32 {\n'
            '    match code {\n'
            '        #[cfg(test)]\n'
            '        Foo { a: _ } | Bar => {\n'
            '            static INSIDE_ARM: OnceLock<u64> = OnceLock::new();\n'
            '            101\n'
            '        }\n'
            '        _ => code,\n'
            '    }\n'
            '}\n'
            '#[cfg(test)]\n'
            'static TEST_BYTES: [u8; 4] = [1, 2, 3, 4];\n'
            '#[cfg(test)]\n'
            'fn helper_with_array_sig(buf: [u8; 4]) -> [u8; 4] {\n'
            '    static HIDDEN_IN_TEST_FN: OnceLock<u64> = OnceLock::new();\n'
            '    buf\n'
            '}\n'
            '#[cfg(test)]\n'
            'mod tests {\n'
            '    static IN_MOD_TEST: AtomicU64 = AtomicU64::new(0);\n'
            '    thread_local! { static TL_TEST: Cell<u64> = const { Cell::new(0) }; }\n'
            '}\n'
        )
        got = [
            (kind, sym)
            for (_ln, kind, sym) in ratchet_checks.scan_process_globals_in_rust_source("eve-server", "sample.rs", sample_src)
        ]
        self.assertEqual(
            got,
            [
                ("static_sync", "MUT_COUNTER"),
                ("static_sync", "MUT_GLOBAL_CELL"),
                ("static_sync", "ONCE_FLAG"),
                ("static_mut", "RAW_MUT"),
                ("static_sync", "WRAPPER_GLOBAL"),
                ("static_sync", "MUTABLE_ARRAY_ELEM"),
                ("static_sync", "MUTABLE_TUPLE_ELEM"),
                ("thread_local", "TL_ONE"),
                ("thread_local", "TL_TWO"),
                ("task_local", "TASK_SCOPE"),
                ("static_sync", "AFTER_STRUCT"),
            ],
        )

        # Audited IMMUTABLE_STATIC_TABLES entry ignored only for exact (crate, file, sym, type)
        self.assertEqual(
            ratchet_checks.scan_process_globals_in_rust_source(
                "eve-proto", "marshal.rs", "static ENCODE_NONE_FALLBACK: PyRep = PyRep::None;\n"
            ),
            [],
        )
        self.assertEqual(
            len(
                ratchet_checks.scan_process_globals_in_rust_source(
                    "eve-server", "marshal.rs", "static ENCODE_NONE_FALLBACK: PyRep = PyRep::None;\n"
                )
            ),
            1,
        )

        # 2. Multi-crate scanning, external #[cfg(test)] mod + #[path = "..."] in both orders, excluded eve-testkit
        with tempfile.TemporaryDirectory() as tmp:
            _write(
                os.path.join(tmp, "crates/eve-server/src/router/entry.rs"),
                '#[cfg(test)]\n#[path = "sub/helper_alpha.rs"]\nmod helper_alpha;\n'
                '#[path = "../helper_beta.rs"]\n#[cfg(test)]\nmod helper_beta;\n'
                '#[cfg(test)]\nmod plain_test_mod;\n',
            )
            _write(os.path.join(tmp, "crates/eve-server/src/router/sub/helper_alpha.rs"), "static T1: OnceLock<u64> = OnceLock::new();\n")
            _write(os.path.join(tmp, "crates/eve-server/src/helper_beta.rs"), "static T2: OnceLock<u64> = OnceLock::new();\n")
            _write(os.path.join(tmp, "crates/eve-server/src/router/entry/plain_test_mod.rs"), "static T3: OnceLock<u64> = OnceLock::new();\n")
            _write(os.path.join(tmp, "crates/eve-testkit/src/lib.rs"), "static TESTKIT_OK: OnceLock<u64> = OnceLock::new();\n")
            _write(os.path.join(tmp, "crates/eve-db/src/store.rs"), "static mut LEAKY_RAW: u64 = 0;\n")
            _write(os.path.join(tmp, "crates/eve-proto/src/codec.rs"), "static NEW_CACHE: OnceLock<Vec<u8>> = OnceLock::new();\n")
            _write(
                os.path.join(tmp, "crates/eve-sde/src/loader.rs"),
                "thread_local! { static BAD_TL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/router/scope.rs"),
                "tokio::task_local! { static BAD_TASK_SCOPE: u64; }\n",
            )
            _write(os.path.join(tmp, ratchet_checks.PROCESS_GLOBAL_REL), json.dumps(_empty_ratchet_doc()))
            viols = ratchet_checks.check_process_global_ratchet(tmp)
            v_blob = "\n".join(v[1] for v in viols)
            for needle in (
                "crates/eve-db/src/store.rs", "static_mut LEAKY_RAW",
                "crates/eve-proto/src/codec.rs", "static_sync NEW_CACHE",
                "crates/eve-sde/src/loader.rs", "thread_local BAD_TL",
                "crates/eve-server/src/router/scope.rs", "task_local BAD_TASK_SCOPE",
            ):
                self.assertIn(needle, v_blob, "Missing %r in %s" % (needle, viols))
            for skipped in ("T1", "T2", "T3", "TESTKIT_OK"):
                self.assertNotIn(skipped, v_blob)

        # 3. Allowlist rationale, kind mismatch, count regression, falling tightening, and #1583 umbrella protection
        with tempfile.TemporaryDirectory() as tmp:
            doc = _empty_ratchet_doc()
            doc["groups"]["A_game_state"]["max_count"] = 2
            doc["groups"]["G_process_wide_allowlist"]["max_count"] = 1
            for g in ratchet_checks.PROCESS_GLOBAL_GROUPS:
                if g not in ("A_game_state", "G_process_wide_allowlist"):
                    doc["groups"][g]["max_count"] = 0
            doc["declarations"] = [
                {
                    "crate_name": "eve-db",
                    "rel_file": "metrics.rs",
                    "symbol": "METRICS_COUNT",
                    "kind": "static_sync",
                    "group": "G_process_wide_allowlist",
                    "max_count": 1,
                    "rationale": "Process-wide metric counter.",
                },
                {
                    "crate_name": "eve-server",
                    "rel_file": "state.rs",
                    "symbol": "GAME_MAP",
                    "kind": "static_sync",
                    "group": "A_game_state",
                    "max_count": 2,
                },
            ]
            _write(os.path.join(tmp, ratchet_checks.PROCESS_GLOBAL_REL), json.dumps(doc, indent=2) + "\n")
            _write(os.path.join(tmp, "crates/eve-db/src/metrics.rs"), "static METRICS_COUNT: AtomicU64 = AtomicU64::new(0);\n")
            _write(
                os.path.join(tmp, "crates/eve-server/src/state.rs"),
                "fn a() { static GAME_MAP: OnceLock<u64> = OnceLock::new(); }\n"
                "fn b() { static GAME_MAP: OnceLock<u64> = OnceLock::new(); }\n",
            )
            self.assertEqual(ratchet_checks.check_process_global_ratchet(tmp), [])
            self.assertEqual(tighten_ratchets.tighten_ratchets(tmp, write=False, targets=("process_global",)), ([], 0, []))
            self.assertIn(1583, ratchet_checks.protected_umbrella_issues(tmp))

            # Commit guard blocks `Fixes #1583` while debt group A_game_state > 0, allows `Refs #1583`
            prot = ratchet_checks.protected_umbrella_issues(tmp)
            self.assertTrue(
                any(
                    v[0] == "UMBRELLA_ISSUE_AUTOCLOSE"
                    for v in ratchet_checks.lint_commit_messages_for_issue_closures(
                        [("1234567890", "fix(arch): remove one global\n\nFixes #1583\n")], prot
                    )
                )
            )
            self.assertEqual(
                ratchet_checks.lint_commit_messages_for_issue_closures(
                    [("1234567890", "fix(arch): remove one global\n\nRefs #1583\n")], prot
                ),
                [],
            )

            # Group G missing or multiline rationale MUST fail validation
            bad_doc = json.loads(json.dumps(doc))
            del bad_doc["declarations"][0]["rationale"]
            self.assertTrue(any("missing required 1-line rationale" in v[1] for v in ratchet_checks.validate_process_global_ratchet_doc(bad_doc)))
            bad_doc["declarations"][0]["rationale"] = "line1\nline2"
            self.assertTrue(any("non-empty single line" in v[1] for v in ratchet_checks.validate_process_global_ratchet_doc(bad_doc)))

            # Kind mismatch fails loudly
            _write(
                os.path.join(tmp, "crates/eve-server/src/state.rs"),
                "thread_local! { static GAME_MAP: Cell<u64> = const { Cell::new(0) }; }\n",
            )
            self.assertTrue(any("kind changed" in v[1] for v in ratchet_checks.check_process_global_ratchet(tmp)))

            # Falling ratchet: reducing GAME_MAP 2 -> 0 passes ratchet_checks, and tighten_ratchets prunes it & lowers group ceiling!
            _write(os.path.join(tmp, "crates/eve-server/src/state.rs"), "// cleaned up!\n")
            self.assertEqual(ratchet_checks.check_process_global_ratchet(tmp), [])
            changed, slack, regs = tighten_ratchets.tighten_ratchets(tmp, write=True, targets=("process_global",))
            self.assertEqual(changed, [ratchet_checks.PROCESS_GLOBAL_REL])
            self.assertEqual(slack, 2)  # 1 pruned declaration + 1 lowered group max_count
            self.assertEqual(regs, [])
            self.assertEqual(tighten_ratchets.tighten_ratchets(tmp, write=False, targets=("process_global",)), ([], 0, []))
            tight_doc = ratchet_checks._read_json(os.path.join(tmp, ratchet_checks.PROCESS_GLOBAL_REL))
            self.assertEqual(tight_doc["groups"]["A_game_state"]["max_count"], 0)
            self.assertEqual([d["symbol"] for d in tight_doc["declarations"]], ["METRICS_COUNT"])
            # Once all A..F debt groups reach 0 (only Group G allowlist remains), #1583 is no longer blocked from closing!
            self.assertNotIn(1583, ratchet_checks.protected_umbrella_issues(tmp))

            # Regression (adding new global) fails hard and tighten_ratchets refuses to raise baseline
            _write(os.path.join(tmp, "crates/eve-server/src/state.rs"), "static NEW_LEAK: OnceLock<u64> = OnceLock::new();\n")
            self.assertTrue(any("NEW_LEAK" in v[1] for v in ratchet_checks.check_process_global_ratchet(tmp)))
            before_raw = open(os.path.join(tmp, ratchet_checks.PROCESS_GLOBAL_REL), encoding="utf-8").read()
            _, _, regs2 = tighten_ratchets.tighten_ratchets(tmp, write=True, targets=("process_global",))
            self.assertTrue(any("NEW_LEAK" in r for r in regs2), regs2)
            self.assertEqual(open(os.path.join(tmp, ratchet_checks.PROCESS_GLOBAL_REL), encoding="utf-8").read(), before_raw)

        # 4. Live repo 1:1 parity verification against checked-in crates/eve-testkit/ratchets/process_global_ratchet.json
        live_doc = ratchet_checks._read_json(os.path.join(REPO_ROOT, ratchet_checks.PROCESS_GLOBAL_REL))
        live_rep, _, _ = ratchet_checks.scan_workspace_process_globals(
            REPO_ROOT, excluded_crates=set(live_doc["excluded_crates"])
        )
        self.assertEqual(
            live_rep["scanned_crates"],
            ["eve-db", "eve-destiny", "eve-dogma", "eve-machonet", "eve-proto", "eve-sde", "eve-server"],
        )
        self.assertFalse(
            any("r6_t1_p1_security_tests_combat" in f[1] for f in live_rep["findings"]),
            "#[cfg(test)] external module files must be skipped",
        )
        self.assertEqual(ratchet_checks.check_process_global_ratchet(REPO_ROOT), [])
        _, _, live_regs = tighten_ratchets.tighten_ratchets(REPO_ROOT, write=False, targets=("process_global",))
        self.assertEqual(live_regs, [])
        if any(
            int(live_doc["groups"][g]["max_count"]) > 0
            for g in ratchet_checks.PROCESS_GLOBAL_DEBT_GROUPS
        ):
            self.assertIn(1583, ratchet_checks.protected_umbrella_issues(REPO_ROOT))
        dump_proc = subprocess.run(
            [sys.executable, os.path.join(SCRIPTS_DIR, "tighten_ratchets.py"), "--root", REPO_ROOT, "--dump-counts-json"],
            capture_output=True,
            text=True,
            check=True,
        )
        dumped = json.loads(dump_proc.stdout)
        self.assertIn("process_global", dumped)
        self.assertEqual(dumped["process_global"]["scanned_crates"], live_rep["scanned_crates"])

    def test_1496_crate_root_and_submodule_src_tests_rs_filters_preserved(self):
        # 1. `_norm_src_stem` maps nested `<mod>/tests.rs` to `<mod>` instead of `"tests"`
        self.assertEqual(run_review._norm_src_stem("foo/tests.rs"), "foo")
        self.assertEqual(run_review._norm_src_stem("services/bar/tests.rs"), "bar")
        self.assertEqual(run_review._norm_src_stem("tests.rs"), "tests")

        def run_for(paths):
            def fake_sh(cmd, cwd, env=None, timeout=None):
                if cmd[:3] == ["git", "diff", "--name-only"]:
                    return 0, "\n".join(paths) + "\n"
                if cmd[:3] == ["git", "diff", "-U0"]:
                    return 0, ""
                return 0, ""

            with mock.patch.object(run_review, "sh", side_effect=fake_sh):
                return run_review.derive_affected_test_commands(REPO_ROOT, "origin/main", "cargo", head="FAKE_SHA")

        # 2. Crate-root `crates/eve-db/src/tests.rs` alongside another submodule (`missions.rs`)
        #    MUST preserve `tests::` (never stripped by `if f != "tests"` at line 1272!)
        db_cmds = run_for(["crates/eve-db/src/tests.rs", "crates/eve-db/src/missions.rs"])
        self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-db", "--lib", "tests::"], db_cmds)
        self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-db", "--lib", "missions"], db_cmds)
        # Verify coalescing preserves both filters into a single `eve-db --lib` invocation
        coalesced = run_review._coalesce_cargo_test_commands(db_cmds)
        self.assertIn(
            ["cargo", "test", "-j", "4", "-p", "eve-db", "--lib", "--", "missions", "tests::"],
            coalesced,
        )

        # 3. Submodule `crates/eve-server/src/foo/tests.rs` selects `foo` (not dropped `"tests"`)
        srv_cmds = run_for(["crates/eve-server/src/foo/tests.rs", "crates/eve-server/src/bar.rs"])
        self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "foo"], srv_cmds)
        self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "bar"], srv_cmds)


if __name__ == "__main__":
    unittest.main()
