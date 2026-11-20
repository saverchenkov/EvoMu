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
                "// parity: rpc:marketProxy.PlaceBuyOrder, feat:new.row\n#[test]\nfn new_t() { assert_eq!(2, 2); }\n",
            )
            got_rows = run_review._collect_diff_parity_rows(tmp, base_sha)
            self.assertEqual(got_rows, {"rpc:marketProxy.PlaceBuyOrder", "feat:new.row"})

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
        # 1. `tools/client-harness/` changes trigger Python unittest discovery + harness IT
        def fake_harness_diff(cmd, cwd, env=None, timeout=None):
            if cmd[:3] == ["git", "diff", "--name-only"]:
                return 0, "tools/client-harness/playtester.py\n"
            return 0, ""

        with mock.patch.object(run_review, "sh", side_effect=fake_harness_diff):
            cmds = run_review.derive_affected_test_commands(REPO_ROOT, "origin/main", "cargo", head="FAKE_SHA")
        self.assertIn(
            [sys.executable, "-m", "unittest", "discover", "-s", "tools/client-harness", "-p", "test_*.py", "-v"],
            cmds,
        )
        self.assertIn(
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", "rpc_dispatch_and_client_harness_e2e"],
            cmds,
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

            # TT-10: `exact: true` enforced UNCONDITIONALLY (without EVOMU_ENFORCE_EXACT_ARCH_DEBT)
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/arch_debt_ratchet.json"),
                json.dumps({
                    "version": 1,
                    "patterns": {
                        "master_wallet": {"max_occurrences": 1, "max_files": 1, "exact": True, "regex": r"\bmaster_wallet\b"},
                    },
                }),
            )
            clean_env = {k: v for k, v in os.environ.items() if k != "EVOMU_ENFORCE_EXACT_ARCH_DEBT"}
            with mock.patch.dict(os.environ, clean_env, clear=True):
                exact_viols = ratchet_checks.check_arch_debt_ratchet(tmp)
            self.assertTrue(any("dropped below exact ratchet" in v[1] for v in exact_viols), exact_viols)

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


if __name__ == "__main__":
    unittest.main()
