from __future__ import annotations

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

TOOLS_DIR = Path(__file__).resolve().parents[1]
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))

import sync_bugs_to_github as sync_mod


class FakeGitHubCLI:
    """In-memory `gh` subprocess mock tracking created issues and posted comments."""

    def __init__(self) -> None:
        self.created_issues = []
        self.comments = []
        self.reopened = []
        self.labels_created = []
        self._next_num = 100

    def _read_body_file(self, cmd: list[str]) -> str:
        idx = cmd.index("--body-file")
        with open(cmd[idx + 1], "r", encoding="utf-8") as fh:
            return fh.read()

    def _option_value(self, cmd: list[str], flag: str) -> str:
        idx = cmd.index(flag)
        return cmd[idx + 1]

    def __call__(self, cmd, **kwargs):  # type: ignore[no-untyped-def]
        if cmd[:3] == ["gh", "label", "create"]:
            self.labels_created.append(cmd[3])
            return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")

        if cmd[:3] == ["gh", "issue", "list"]:
            term = self._option_value(cmd, "--search")
            matches = []
            for issue in self.created_issues:
                # Match the quoted search fragment inside `--search` (`"..." in:body`).
                fragment = term.split('"')[1] if '"' in term else term
                if fragment in issue["body"]:
                    matches.append(
                        {
                            "number": issue["number"],
                            "title": issue["title"],
                            "state": issue["state"],
                        }
                    )
                    break
            return subprocess.CompletedProcess(cmd, 0, stdout=json.dumps(matches), stderr="")

        if cmd[:3] == ["gh", "issue", "create"]:
            self._next_num += 1
            num = self._next_num
            body = self._read_body_file(cmd)
            title = self._option_value(cmd, "--title")
            labels = self._option_value(cmd, "--label").split(",") if "--label" in cmd else []
            url = f"https://github.com/saverchenkov/EvoMu/issues/{num}"
            self.created_issues.append(
                {
                    "number": num,
                    "title": title,
                    "labels": labels,
                    "body": body,
                    "state": "OPEN",
                    "url": url,
                }
            )
            return subprocess.CompletedProcess(cmd, 0, stdout=url + "\n", stderr="")

        if cmd[:3] == ["gh", "issue", "comment"]:
            num = int(cmd[3])
            body = self._read_body_file(cmd)
            self.comments.append({"number": num, "body": body})
            return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")

        if cmd[:3] == ["gh", "issue", "reopen"]:
            num = int(cmd[3])
            self.reopened.append(num)
            for issue in self.created_issues:
                if issue["number"] == num:
                    issue["state"] = "OPEN"
            return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")

        raise AssertionError(f"Unexpected command invoked in test: {cmd}")


def _make_crash_bundle(
    *,
    stack_hash: int,
    trace: str,
    stack_key: str = "",
    flight_recorder: list | None = None,
    server_logs_before: list | None = None,
    server_logs_after: list | None = None,
    session: dict | None = None,
    extra: dict | None = None,
) -> dict:
    bundle = {
        "schema_version": 1,
        "report_source": "exception",
        "timestamp": "2026-11-18T12:00:00Z",
        "stack_hash": stack_hash,
        "stack_key": stack_key,
        "trace": trace,
        "session": (
            {
                "character_id": 2100000001,
                "character_name": "Playtester Alpha",
                "solar_system_id": 30000142,
                "solar_system_name": "Jita",
                "ship_type_name": "Rifter",
                "station_id": 60003760,
            }
            if session is None
            else session
        ),
        "flight_recorder": [] if flight_recorder is None else flight_recorder,
        "server_logs_before": [] if server_logs_before is None else server_logs_before,
        "server_logs_after": [] if server_logs_after is None else server_logs_after,
    }
    if extra:
        bundle.update(extra)
    return bundle


def _make_zero_gateway_bundles_1096_to_1118() -> list[dict]:
    """Builds 12 realistic `#1096..#1118`-style bundles where MachoNet worked but 0 gateway requests reached the server."""
    macho_calls = [
        {"offset_ms": -420, "service": "invbroker", "method": "GetInventory", "status": "OK", "duration_micros": 1100},
        {"offset_ms": -120, "service": "alert", "method": "SendClientStackTraceAlert", "status": "OK", "duration_micros": 400},
    ]
    macho_logs = [
        "2026-11-16T08:00:00Z INFO eve_server::router::dispatch: [invbroker] GetInventory ok",
    ]
    specs = [
        (
            1096,
            "skin_browser.py:55",
            'Traceback (most recent call last):\n  File "packages/cosmetics/skin_browser.py", line 55, in Open\n'
            '  File "packages/eve_public/quasar.py", line 58, in send_request\n'
            "ServerException: ServerException(status_code=500, status_message=invalid character on request) "
            'response_wrapper = dispatched { seconds: 1794158000 nanos: 100 } [Quasar descriptor note]',
        ),
        (
            1101,
            "nes_store.py:112",
            'Traceback (most recent call last):\n  File "packages/vgs/nes_store.py", line 112, in FetchOffers\n'
            "ServerException: ServerException(status_code=500, status_message=invalid character on request) "
            "response_wrapper = dispatched { seconds: 1794158005 nanos: 200 }",
        ),
        (
            1103,
            "quasar_catalog.py:90",
            'Traceback (most recent call last):\n  File "packages/eve_public/quasar.py", line 58, in _dispatch\n'
            "ServerException: ServerException(status_code=500, status_message=invalid character on request)",
        ),
        (
            1106,
            "ship_skins.py:210",
            'Traceback (most recent call last):\n  File "packages/cosmetics/ship_skins.py", line 210, in GetLicenses\n'
            '  File "packages/eve_public/quasar.py", line 91, in call\n'
            "RuntimeError: Could not fetch owned ship cosmetic licenses: ServerException(status_code=500)",
        ),
        (
            1107,
            "evermarks.py:42",
            'Traceback (most recent call last):\n  File "packages/lp/evermarks.py", line 42, in Balance\n'
            "ServerException: ServerException(status_code=500, status_message=invalid character on request)",
        ),
        (
            1108,
            "plex_vault.py:88",
            'Traceback (most recent call last):\n  File "packages/plex/plex_vault.py", line 88, in Refresh\n'
            '  File "packages/publicgateway/request_messenger.py", line 140, in Send\n'
            "GenericInternalServerError: Request: Get PLEX balance: 500 - InternalServerError - Request timed out.",
        ),
        (
            1109,
            "plex_market.py:19",
            'Traceback (most recent call last):\n  File "packages/plex/plex_market.py", line 19, in OpenWallet\n'
            "GenericInternalServerError: Request: Get PLEX balance: 500 - InternalServerError - Request timed out.",
        ),
        (
            1110,
            "mercenary_den.py:73",
            'Traceback (most recent call last):\n  File "packages/sov/mercenary_den.py", line 73, in Load\n'
            "ServerException: ServerException(status_code=500, status_message=invalid character on request)",
        ),
        (
            1111,
            "freelance_jobs.py:144",
            'Traceback (most recent call last):\n  File "packages/jobs/freelance_jobs.py", line 144, in List\n'
            "ServerException: ServerException(status_code=500, status_message=invalid character on request)",
        ),
        (
            1112,
            "air_opportunities.py:301",
            'Traceback (most recent call last):\n  File "packages/air/air_opportunities.py", line 301, in Fetch\n'
            "ServerException: ServerException(status_code=500, status_message=invalid character on request)",
        ),
        (
            1113,
            "daily_goals.py:64",
            'Traceback (most recent call last):\n  File "packages/campaigns/daily_goals.py", line 64, in Claim\n'
            "CampaignRequestError: Public gateway campaign request failed",
        ),
        (
            1118,
            "paragon_hub.py:215",
            'Traceback (most recent call last):\n  File "packages/paragon/paragon_hub.py", line 215, in QueryEmblems\n'
            "ServerException: ServerException(status_code=500, status_message=invalid character on request)",
        ),
    ]
    return [
        _make_crash_bundle(
            stack_hash=-(100000 + issue_id),
            stack_key=loc,
            trace=trace,
            flight_recorder=list(macho_calls),
            server_logs_before=list(macho_logs),
        )
        for issue_id, loc, trace in specs
    ]


class SyncBugsTriageDedupTests(unittest.TestCase):
    def test_twelve_zero_server_request_tracebacks_produce_one_issue_and_comment_tracebacks(self) -> None:
        bundles = [
            _make_crash_bundle(
                stack_hash=-(5000 + idx),
                stack_key=f"mod_{idx}.py:{10 + idx}",
                trace=(
                    f'Traceback (most recent call last):\n'
                    f'  File "eve/client/script/ui/mod_{idx}.py", line {10 + idx}, in Handler{idx}\n'
                    f"RuntimeError: distinct failure #{idx}"
                ),
                session={},
                extra={"server_request_count": 0},
            )
            for idx in range(12)
        ]

        fake_gh = FakeGitHubCLI()
        with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
            sync_mod.subprocess, "run", side_effect=fake_gh
        ):
            ensured: set[str] = set()
            issue_ids = [
                sync_mod.create_or_update_issue("saverchenkov/EvoMu", b, 1, ensured, dry_run=False)
                for b in bundles
            ]

        self.assertEqual(len(set(issue_ids)), 1)
        self.assertEqual(len(fake_gh.created_issues), 1)
        self.assertEqual(len(fake_gh.comments), 11)
        created = fake_gh.created_issues[0]
        self.assertTrue(created["title"].startswith("[Environment]"))
        self.assertIn("environment", created["labels"])
        self.assertIn("Session Server Requests:** `0`", created["body"])
        self.assertIn("distinct failure #0", created["body"])
        for idx in range(1, 12):
            self.assertIn(f"distinct failure #{idx}", fake_gh.comments[idx - 1]["body"])

    def test_empty_telemetry_without_explicit_count_collapses_crashes_as_zero_server_requests(self) -> None:
        b1 = _make_crash_bundle(
            stack_hash=-111,
            trace='Traceback (most recent call last):\n  File "a.py", line 1, in f\nKeyError: "x"',
            session={},
        )
        b2 = _make_crash_bundle(
            stack_hash=-222,
            trace='Traceback (most recent call last):\n  File "b.py", line 2, in g\nValueError: "y"',
            session={},
        )
        self.assertEqual(sync_mod._effective_bundle_hash(b1), sync_mod._effective_bundle_hash(b2))
        title, labels = sync_mod.build_issue_title_and_labels(b1)
        self.assertIn("Zero server requests", title)
        self.assertIn("environment", labels)

    def test_mismatched_client_and_server_build_produces_one_issue_and_comments(self) -> None:
        bundles = [
            _make_crash_bundle(
                stack_hash=-(7000 + idx),
                trace=(
                    f'Traceback (most recent call last):\n'
                    f'  File "eve/client/script/ui/view_{idx}.py", line {20 + idx}, in Open\n'
                    f"AttributeError: missing attr_{idx}"
                ),
                extra={
                    "handshake": {"client_build": 3698400, "server_build": 3698585},
                    "server_request_count": 0,
                },
            )
            for idx in range(5)
        ]

        fake_gh = FakeGitHubCLI()
        with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
            sync_mod.subprocess, "run", side_effect=fake_gh
        ):
            ensured: set[str] = set()
            for b in bundles:
                sync_mod.create_or_update_issue("saverchenkov/EvoMu", b, 1, ensured, dry_run=False)

        self.assertEqual(len(fake_gh.created_issues), 1)
        self.assertEqual(len(fake_gh.comments), 4)
        created = fake_gh.created_issues[0]
        self.assertEqual(
            created["title"],
            "[Environment] Client/server build mismatch (client=3698400, server=3698585)",
        )
        self.assertIn("environment", created["labels"])
        self.assertIn("Handshake Client Build:** `3698400`", created["body"])
        self.assertIn("Server Build:** `3698585`", created["body"])
        for idx in range(1, 5):
            self.assertIn(f"missing attr_{idx}", fake_gh.comments[idx - 1]["body"])

    def test_log_derived_build_mismatch_and_non_mismatch_negative_case(self) -> None:
        log_mismatch_bundle = _make_crash_bundle(
            stack_hash=-8123,
            trace='Traceback (most recent call last):\n  File "login.py", line 10, in Go\nRuntimeError: disconnected',
            server_logs_before=[
                "2026-11-18T12:00:00Z WARN eve_machonet::handshake: Handshake Failed - Boot Build Mismatch (Boot Build is 3698100 but should be at least 3698585)"
            ],
        )
        fault = sync_mod.classify_environment_fault(log_mismatch_bundle)
        self.assertIsNotNone(fault)
        assert fault is not None
        self.assertEqual(fault[0], "build_mismatch")
        self.assertEqual(fault[1], "env:build_mismatch:client=3698100:server=3698585")

        matching_bundle = _make_crash_bundle(
            stack_hash=-8124,
            trace='Traceback (most recent call last):\n  File "map.pyj", line 412, in Open\nTypeError: bad',
            flight_recorder=[{"service": "map", "method": "GetSystems", "status": "OK", "duration_micros": 100}],
            extra={"handshake": {"client_build": 3698585, "server_build": "3698585"}},
        )
        self.assertIsNone(sync_mod.classify_environment_fault(matching_bundle))

    def test_same_exception_from_client_crash_and_client_python_report_collapses(self) -> None:
        crash_bundle = _make_crash_bundle(
            stack_hash=-514675905,
            stack_key="",
            trace=(
                "EXCEPTION #26 logged at 11/16/2026 11:24:30 : unhandled exception\n\n"
                "Formatted exception info:\n"
                "RuntimeError: Cannot sleep for more than 24 hours. 86400 seconds is the maximum.\n\n"
                "Thrown at:\n"
                "<pre>/app/client/eve/client/script/ui/services/godma.py(1782) ActualStopEffect\n"
                "EXCEPTION END\n"
            ),
            flight_recorder=[{"service": "godma", "method": "GetState", "status": "OK", "duration_micros": 500}],
        )
        playtester_bundle = {
            "report_source": "f12_manual",
            "timestamp": "2026-11-16T11:24:30Z",
            "stack_hash": -7386150587313191239,
            "stack_key": "playtester:client-python:client_traceback",
            "step": 41995,
            "trace": (
                "logmodule.LogException Traceback (most recent call last):\n"
                '  File "C:\\client\\eve\\client\\script\\ui\\services\\godma.py", line 1782, in ActualStopEffect\n'
                "    blue.pyos.synchro.SleepWallclock(ms)\n"
                "RuntimeError: Cannot sleep for more than 24 hours. 86400 seconds is the maximum.\n"
            ),
            "manual_report": {
                "report_key": "playtester_step41995_abcd",
                "category_id": "client-python",
                "category_name": "client_traceback",
                "title": "python-error in godma.py:1782",
                "description": "Hit sleep error during step 41995",
            },
            "flight_recorder": [{"service": "godma", "method": "GetState", "status": "OK", "duration_micros": 500}],
            "session": {"character_id": 2100000001, "solar_system_name": "Jita"},
        }

        self.assertEqual(
            sync_mod._effective_bundle_hash(dict(crash_bundle)),
            sync_mod._effective_bundle_hash(dict(playtester_bundle)),
        )

        # Verify both arrival orders produce exactly 1 GitHub issue sequentially.
        for first, second in ((crash_bundle, playtester_bundle), (playtester_bundle, crash_bundle)):
            fake_gh = FakeGitHubCLI()
            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=fake_gh
            ):
                ensured: set[str] = set()
                id1 = sync_mod.create_or_update_issue("saverchenkov/EvoMu", dict(first), 1, ensured, dry_run=False)
                id2 = sync_mod.create_or_update_issue("saverchenkov/EvoMu", dict(second), 2, ensured, dry_run=False)
            self.assertEqual(id1, id2)
            self.assertEqual(len(fake_gh.created_issues), 1)
            self.assertEqual(len(fake_gh.comments), 1)

        # Verify LogTraceback + CALLSTACK + wrapper frame (like #1140 + #1141) also collapses.
        log_tb_crash = _make_crash_bundle(
            stack_hash=-514675905,
            trace=(
                "STACKTRACE #6 logged at 11/16/2026 11:19:19 : Unexpectedly got shipID = None\n\n"
                "Traced at:\n"
                "/app/client/eve/client/script/ui/shared/fitting/clientDogmaLocation.py(275) _MakeShipActive\n"
                "/app/client/evomu_site.py(131) _wrapped\n"
            ),
            flight_recorder=[{"service": "dogmaIM", "method": "GetLocation", "status": "OK", "duration_micros": 300}],
        )
        log_tb_f12 = {
            "report_source": "f12_manual",
            "stack_hash": -7386150587313191999,
            "stack_key": "playtester:client-python:client_traceback",
            "step": 41970,
            "trace": (
                "log.LogTraceback args=('Unexpectedly got shipID = None',) kwargs={}\n"
                "None\n\n"
                "CALLSTACK:\n"
                '  File "C:/client/eve/client/script/ui/shared/fitting/clientDogmaLocation.py", line 275, in _MakeShipActive\n'
            ),
            "manual_report": {
                "report_key": "playtester_step41970_efgh",
                "category_id": "client-python",
                "category_name": "client_traceback",
                "title": "python-error in clientDogmaLocation.py:275",
            },
            "flight_recorder": [{"service": "dogmaIM", "method": "GetLocation", "status": "OK", "duration_micros": 300}],
            "session": {"character_id": 2100000001},
        }
        self.assertEqual(
            sync_mod._effective_bundle_hash(log_tb_crash),
            sync_mod._effective_bundle_hash(log_tb_f12),
        )

        # Verify batch dry-run collapses the pair into one created issue + one comment.
        with tempfile.TemporaryDirectory() as tmpdir:
            qdir = Path(tmpdir)
            (qdir / "01_f12.json").write_text(json.dumps(playtester_bundle), encoding="utf-8")
            (qdir / "02_crash.json").write_text(json.dumps(crash_bundle), encoding="utf-8")
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                rc = sync_mod.main(["--queue-dir", str(qdir), "--dry-run"])
            out = buf.getvalue()
            self.assertEqual(rc, 0)
            self.assertEqual(out.count("[dry-run] Would create issue with title:"), 1)
            self.assertEqual(out.count("[dry-run] Would post comment to #999999:"), 1)
            self.assertIn("[Client Crash] RuntimeError: Cannot sleep for more than 24 hours.", out)

    def test_replay_1096_to_1118_zero_gateway_requests_dry_run_yields_one_env_issue(self) -> None:
        bundles = _make_zero_gateway_bundles_1096_to_1118()
        self.assertEqual(len(bundles), 12)

        with tempfile.TemporaryDirectory() as tmpdir:
            qdir = Path(tmpdir)
            for idx, bundle in enumerate(bundles):
                (qdir / f"bundle_{idx:02d}.json").write_text(json.dumps(bundle), encoding="utf-8")

            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                rc = sync_mod.main(["--queue-dir", str(qdir), "--dry-run"])
            out = buf.getvalue()

        self.assertEqual(rc, 0)
        self.assertEqual(out.count("[dry-run] Would create issue with title:"), 1)
        self.assertEqual(out.count("[dry-run] Would post comment to #999999:"), 11)
        self.assertIn(
            "[Environment] Zero gateway requests reached server (misconfigured gateway) (labels: bug,environment)",
            out,
        )
        self.assertIn("CampaignRequestError", out)
        self.assertIn("Get PLEX balance", out)
        self.assertIn("Processed 12/12 incident file(s)", out)

    def test_gateway_request_reaching_server_is_not_classified_as_zero_gateway_requests(self) -> None:
        server_seen_bundle = _make_crash_bundle(
            stack_hash=-99001,
            trace='Traceback (most recent call last):\n  File "packages/cosmetics/skin_browser.py", line 55, in Open\n'
            "ServerException: ServerException(status_code=500, status_message=server internal failure)",
            flight_recorder=[{"service": "invbroker", "method": "GetInventory", "status": "OK", "duration_micros": 100}],
            server_logs_before=[
                "2026-11-16T08:00:00Z WARN eve_server::quasar_grpc: Quasar gRPC public request returned non-200 status",
            ],
        )
        self.assertIsNone(sync_mod.classify_environment_fault(server_seen_bundle))

    def test_manual_ui_report_and_mollycoddled_uthread_regressions(self) -> None:
        # Matches `client_bug_reporting_suite.rs` manual fixture expectations.
        manual_ui = {
            "report_source": "f12_manual",
            "stack_hash": 987654321,
            "stack_key": "UI",
            "trace": "",
            "manual_report": {
                "report_key": "EBR-0001-ABCDEF",
                "category_id": "UI",
                "title": "Capacitor ring flickers",
                "client_build": 3698585,
            },
            "flight_recorder": [],
            "server_logs_before": [],
            "server_logs_after": [],
        }
        self.assertIsNone(sync_mod.classify_environment_fault(manual_ui))
        title, labels = sync_mod.build_issue_title_and_labels(manual_ui)
        self.assertEqual(title, "[Bug Report][UI] Capacitor ring flickers")
        self.assertEqual(labels, ["bug", "player-reported"])

        # Verify `_effective_bundle_hash` is strictly idempotent on MollycoddledUthread even without trace.
        molly = {"stack_hash": -1040548311, "stack_key": "MollycoddledUthread in foo.py", "trace": ""}
        h1 = sync_mod._effective_bundle_hash(molly)
        h2 = sync_mod._effective_bundle_hash(molly)
        self.assertEqual(h1, h2)
        self.assertNotEqual(h1, -1040548311)

    def test_bf1344_01_evomu_site_log_exception_prefers_throw_site_over_callstack(self) -> None:
        trace_a = (
            "log.general.LogException args=(RuntimeError('missing attribute: foo'),) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/inflight/scanner.py", line 412, in DoScan\n'
            "    self.ScanArea()\n"
            "RuntimeError: missing attribute: foo\n"
            "CALLSTACK:\n"
            '  File "carbon/common/lib/uthread2.py", line 111, in wrapper\n'
            "    res = {'k': 'val: with colon'}\n"
        )
        exc_class, msg, loc = sync_mod.parse_traceback_summary(trace_a, "")
        self.assertEqual(exc_class, "RuntimeError")
        self.assertEqual(msg, "missing attribute: foo")
        self.assertEqual(loc, "scanner.py:412 in DoScan")

        trace_b = (
            "log.general.LogException args=(RuntimeError('missing attribute: foo'),) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/neocom/wallet.py", line 88, in OpenWallet\n'
            "    self.LoadBalance()\n"
            "RuntimeError: missing attribute: foo\n"
            "CALLSTACK:\n"
            '  File "carbon/common/lib/uthread2.py", line 111, in wrapper\n'
            "    res = {'k': 'val: with colon'}\n"
        )
        bundle_a = _make_crash_bundle(stack_hash=-6001, trace=trace_a)
        bundle_b = _make_crash_bundle(stack_hash=-6002, trace=trace_b)
        trace_a_alt_callstack = (
            "log.general.LogException args=(RuntimeError('missing attribute: foo'),) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/inflight/scanner.py", line 412, in DoScan\n'
            "    self.ScanArea()\n"
            "RuntimeError: missing attribute: foo\n"
            "CALLSTACK:\n"
            '  File "carbon/common/lib/other_caller.py", line 999, in OtherEntry\n'
            "    x = {'a': 1}\n"
        )
        bundle_a_alt = _make_crash_bundle(stack_hash=-6003, trace=trace_a_alt_callstack)
        self.assertEqual(
            sync_mod._effective_bundle_hash(bundle_a),
            sync_mod._effective_bundle_hash(bundle_a_alt),
        )
        self.assertNotEqual(
            sync_mod._effective_bundle_hash(bundle_a),
            sync_mod._effective_bundle_hash(bundle_b),
        )

    def test_bf1344_01_log_traceback_args_variants_and_callstack_fallback(self) -> None:
        trace_mod = (
            "logmodule.LogTraceback args=('channel open failed: timeout=30s',) kwargs={}\n"
            "CALLSTACK:\n"
            '  File "eve/client/script/ui/shared/neocom/wallet.py", line 88, in OpenWallet\n'
        )
        self.assertEqual(
            sync_mod.parse_traceback_summary(trace_mod, ""),
            ("LogTraceback", "channel open failed: timeout=30s", "wallet.py:88 in OpenWallet"),
        )

        trace_qual = (
            "service.CoreMixIn.LogTraceback args=('service crash: code 42', 'extra') kwargs={'verbose': True}\n"
            "CALLSTACK:\n"
            '  File "eve/client/script/ui/shared/agency.py", line 19, in Load\n'
        )
        self.assertEqual(
            sync_mod.parse_traceback_summary(trace_qual, ""),
            ("LogTraceback", "service crash: code 42 extra", "agency.py:19 in Load"),
        )

        trace_logexc = (
            "log.general.LogException args=('scan aborted: no probe',) kwargs={}\n"
            "NoneType: None\n"
            "CALLSTACK:\n"
            '  File "eve/client/script/ui/probe.py", line 55, in Launch\n'
        )
        self.assertEqual(
            sync_mod.parse_traceback_summary(trace_logexc, ""),
            ("LogException", "scan aborted: no probe", "probe.py:55 in Launch"),
        )

    def test_bf1344_01_pre_frame_spaces_and_pyj_pyc_wrappers(self) -> None:
        trace_pre = (
            "<pre>C:/Program Files (x86)/EVE Online (Dev)/eve/client/script/ui/my window.py(88) OnOpen\n"
            "<pre>C:/Program Files (x86)/EVE Online (Dev)/eve/common/lib/moniker.pyj(14) Call\n"
            "<pre>C:/Program Files (x86)/EVE Online (Dev)/evomu_site.pyc(120) _wrap\n"
            "KeyError: 'foo'\n"
        )
        self.assertEqual(
            sync_mod.parse_traceback_summary(trace_pre, ""),
            ("KeyError", "'foo'", "my window.py:88 in OnOpen"),
        )

        trace_pyj = (
            "<pre>/Applications/EVE Online.app/Contents/eve/client/script/ui/shipui.pyj(312) DoWarp\n"
            "<pre>/Applications/EVE Online.app/Contents/bluepyimpl.pyc(44) __call__\n"
            "RuntimeError: warp failed\n"
        )
        self.assertEqual(
            sync_mod.parse_traceback_summary(trace_pyj, ""),
            ("RuntimeError", "warp failed", "shipui.pyj:312 in DoWarp"),
        )

        # Exception line containing `<file>.py(N)` without `<pre>` must NOT be misclassified as a stack frame (GH1358-06).
        trace_exc_with_py_ref = (
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/handler.py", line 17, in Invoke\n'
            "RuntimeError: failed in /app/foo.py(42)\n"
        )
        self.assertEqual(
            sync_mod.parse_traceback_summary(trace_exc_with_py_ref, ""),
            ("RuntimeError", "failed in /app/foo.py(42)", "handler.py:17 in Invoke"),
        )

    def test_bf1344_02_noisy_server_logs_ignored_and_session_evidence_prevents_zero_server_requests(self) -> None:
        tb = 'Traceback (most recent call last):\n  File "eve/client/ui/test.py", line 12, in Run\nRuntimeError: boom'
        noisy_pre_auth = _make_crash_bundle(
            stack_hash=-7001,
            trace=tb,
            session={},
            server_logs_before=[f"INFO server_tick line {i}" for i in range(25)],
            server_logs_after=["INFO cleanup done"],
        )
        fault = sync_mod.classify_environment_fault(noisy_pre_auth)
        self.assertIsNotNone(fault)
        assert fault is not None
        self.assertEqual(fault[0], "zero_server_requests")

        # Authenticated session / ledger / destiny evidence proves client reached server even with 0 step RPCs.
        authenticated_cases = [
            {"session": {"character_id": 2100000001}},
            {"session": {"user_id": 1001}},
            {"session": {"userid": 1002}},
            {"session": {}, "extra": {"outbound_ledger": [{"service": "machoNet", "method": "GetTime"}]}},
            {"session": {}, "extra": {"destiny_stamps": [{"stamp": 42}]}},
        ]
        for idx, case in enumerate(authenticated_cases):
            extra = dict(case.get("extra") or {})
            extra["server_request_count"] = 0
            bundle = _make_crash_bundle(
                stack_hash=-(7100 + idx),
                trace=tb,
                session=case["session"],
                extra=extra,
            )
            self.assertIsNone(
                sync_mod.classify_environment_fault(bundle),
                f"Authenticated evidence case {case} was misclassified as env fault",
            )
            title, labels = sync_mod.build_issue_title_and_labels(bundle)
            self.assertTrue(title.startswith("[Client Crash]"), f"Unexpected title: {title}")
            self.assertNotIn("environment", labels)

    def test_bf1344_02_non_python_f12_manual_report_not_collapsed_as_zero_server_requests(self) -> None:
        manual_ui_bundle = _make_crash_bundle(
            stack_hash=0,
            trace="",
            session={},
            extra={
                "report_source": "f12_manual",
                "server_request_count": 0,
                "manual_report": {
                    "category_id": "UI",
                    "title": "Hangar overlay clips window header",
                    "description": "Visual overlap after resizing client window.",
                },
            },
        )
        self.assertIsNone(sync_mod.classify_environment_fault(manual_ui_bundle))
        title, labels = sync_mod.build_issue_title_and_labels(manual_ui_bundle)
        self.assertEqual(title, "[Bug Report][UI] Hangar overlay clips window header")
        self.assertIn("player-reported", labels)
        self.assertNotIn("environment", labels)

    def test_bf1344_03_build_mismatch_compatible_client_vs_stale_client(self) -> None:
        tb = 'Traceback (most recent call last):\n  File "eve/client/ui/map.py", line 9, in Open\nValueError: bad map'
        compatible = _make_crash_bundle(
            stack_hash=-8001,
            trace=tb,
            extra={
                "server_request_count": 5,
                "handshake": {
                    "client_boot_build": 3699000,
                    "server_boot_build": 3698585,
                },
            },
        )
        self.assertIsNone(sync_mod.classify_environment_fault(compatible))

        stale = _make_crash_bundle(
            stack_hash=-8002,
            trace=tb,
            extra={
                "server_request_count": 0,
                "handshake": {
                    "client_boot_build": 3600000,
                    "server_boot_build": 3698585,
                },
            },
        )
        fault = sync_mod.classify_environment_fault(stale)
        self.assertIsNotNone(fault)
        assert fault is not None
        self.assertEqual(fault[0], "build_mismatch")
        self.assertEqual(fault[1], "env:build_mismatch:client=3600000:server=3698585")

        # `handshake.build_mismatch: False` (especially when handshake lacks `client_boot_build`) must NOT mask numeric mismatch.
        masked_unknown = _make_crash_bundle(
            stack_hash=-8003,
            trace=tb,
            extra={
                "server_request_count": 5,
                "handshake": {
                    "server_boot_build": 3698585,
                    "build_mismatch": False,
                },
                "manual_report": {"client_build": 3600000},
            },
        )
        fault_masked_unknown = sync_mod.classify_environment_fault(masked_unknown)
        self.assertIsNotNone(fault_masked_unknown)
        assert fault_masked_unknown is not None
        self.assertEqual(fault_masked_unknown[0], "build_mismatch")

        masked_numeric = _make_crash_bundle(
            stack_hash=-8004,
            trace=tb,
            extra={
                "server_request_count": 5,
                "handshake": {
                    "client_boot_build": 3600000,
                    "server_boot_build": 3698585,
                    "build_mismatch": False,
                },
            },
        )
        fault_masked_numeric = sync_mod.classify_environment_fault(masked_numeric)
        self.assertIsNotNone(fault_masked_numeric)
        assert fault_masked_numeric is not None
        self.assertEqual(fault_masked_numeric[0], "build_mismatch")

    def test_bf1344_03_public_gateway_client_transport_error_and_startup_log_still_zero_gateway_requests(self) -> None:
        for gw_extra in ({}, {"gateway_request_count": 0}):
            bundle = _make_crash_bundle(
                stack_hash=-8101,
                trace="quasar_grpc channel failed: Connection refused",
                flight_recorder=[
                    {"kind": "quasar", "service": "publicGateway", "method": "Send", "error": "Connection refused"},
                    {
                        "kind": "quasar",
                        "service": "publicGateway",
                        "method": "Publish",
                        "status": "ERR(status=500 Request timed out)",
                    },
                ],
                server_logs_before=["2026-11-19T12:00:00Z INFO eve_server::quasar_grpc: listening on 127.0.0.1:50051"],
                extra={"server_request_count": 10, **gw_extra},
            )
            fault = sync_mod.classify_environment_fault(bundle)
            self.assertIsNotNone(fault, f"expected zero_gateway_requests for gw_extra={gw_extra}")
            assert fault is not None
            self.assertEqual(fault[0], "zero_gateway_requests")

    def test_bf1344_03_server_reached_quasar_failure_not_collapsed_as_zero_gateway_requests(self) -> None:
        base_trace = "quasar gateway returned UNAUTHENTICATED for mission payload"
        b_explicit = _make_crash_bundle(
            stack_hash=-8201,
            trace=base_trace,
            extra={"server_request_count": 10, "gateway_request_count": 2},
        )
        self.assertIsNone(sync_mod.classify_environment_fault(b_explicit))

        for gw_extra in ({}, {"gateway_request_count": 0}):
            b_flight_ok = _make_crash_bundle(
                stack_hash=-8202,
                trace=base_trace,
                flight_recorder=[{"service": "publicGatewaySvc", "method": "SendRequest", "status": "OK"}],
                extra={"server_request_count": 10, **gw_extra},
            )
            self.assertIsNone(
                sync_mod.classify_environment_fault(b_flight_ok),
                f"gateway_request_count={gw_extra} shadowed observed flight_recorder call",
            )

            b_srv_log = _make_crash_bundle(
                stack_hash=-8203,
                trace=base_trace,
                server_logs_before=[
                    "2026-11-19T12:00:05Z WARN eve_server::quasar_grpc: /eve_public.gateway.Requests/Send -> UNAUTHENTICATED"
                ],
                extra={"server_request_count": 10, **gw_extra},
            )
            self.assertIsNone(
                sync_mod.classify_environment_fault(b_srv_log),
                f"gateway_request_count={gw_extra} shadowed observed server log warning",
            )

    def test_bf1344_01_eve_pre_frame_does_not_shadow_log_args_or_py_frames(self) -> None:
        # 1. LogException args containing `foo.py(42)` must still extract fallback message.
        trace_with_py_ref = (
            "Traceback (most recent call last):\n"
            '  File "packages/ui/window.py", line 88, in Open\n'
            'logmodule.LogException args=("Failed in helper.py(42)",)\n'
        )
        exc_cls, msg, loc = sync_mod.parse_traceback_summary(trace_with_py_ref, "FallbackKey")
        self.assertEqual(exc_cls, "LogException")
        self.assertEqual(msg, "Failed in helper.py(42)")
        self.assertEqual(loc, "window.py:88 in Open")

        # 2. Inline prose/STACKTRACE lines with `.py(N)` outside Thrown at:/Traced at:
        # must not override real `File "...", line N` frames.
        trace_mixed = (
            "Traceback (most recent call last):\n"
            '  File "packages/ui/real_module.py", line 19, in Render\n'
            "    return draw()\n"
            "STACKTRACE #1 decoy.py(999)\n"
            "RuntimeError: broken while calling foo.py(42)\n"
        )
        exc_cls2, msg2, loc2 = sync_mod.parse_traceback_summary(trace_mixed, "FallbackKey")
        self.assertEqual(exc_cls2, "RuntimeError")
        self.assertEqual(msg2, "broken while calling foo.py(42)")
        self.assertEqual(loc2, "real_module.py:19 in Render")

        # 3. Real `<pre>` frames including `<module>` and trailing `</pre>` still parse cleanly.
        trace_pre = (
            "Thrown at:\n"
            "<pre>e:\\Program Files (x86)\\EVE\\packages\\boot.py(12) <module></pre>\n"
            "KeyError: 'id'\n"
        )
        exc_cls3, msg3, loc3 = sync_mod.parse_traceback_summary(trace_pre, "FallbackKey")
        self.assertEqual(exc_cls3, "KeyError")
        self.assertEqual(msg3, "'id'")
        self.assertEqual(loc3, "boot.py:12 in <module>")

    def test_bf1344_03_kind_quasar_recognized_without_gateway_name(self) -> None:
        # `kind == "quasar"` identifies a gateway call even when service/method omit "quasar"/"gateway".
        b_ok = _make_crash_bundle(
            stack_hash=9901,
            trace=(
                "Traceback (most recent call last):\n"
                '  File "char.py", line 10, in Create\n'
                "GatewayError: public gateway RPC response invalid\n"
            ),
            flight_recorder=[
                {
                    "kind": "quasar",
                    "service": "eve_public.character.api.CreateRequest",
                    "method": "Send",
                    "status": "OK",
                }
            ],
            extra={"server_request_count": 5, "gateway_request_count": 0},
        )
        self.assertIsNone(sync_mod.classify_environment_fault(b_ok))

        b_err = _make_crash_bundle(
            stack_hash=9902,
            trace=(
                "Traceback (most recent call last):\n"
                '  File "char.py", line 10, in Create\n'
                "RuntimeError: service unavailable\n"
            ),
            flight_recorder=[
                {
                    "kind": "quasar",
                    "service": "eve_public.character.api.CreateRequest",
                    "method": "Send",
                    "status": "UNAVAILABLE",
                    "error": "DNS resolution failed",
                }
            ],
            extra={"server_request_count": 5, "gateway_request_count": 0},
        )
        fault = sync_mod.classify_environment_fault(b_err)
        self.assertIsNotNone(fault)
        assert fault is not None
        self.assertEqual(fault[0], "zero_gateway_requests")
        self.assertEqual(fault[1], "env:zero_gateway_requests")

    def test_gh1358_r3_02_non_positive_client_build_never_classifies_as_build_mismatch(self) -> None:
        for bad_build in (0, "0", -1, "-42", " 0 "):
            f12_bundle = _make_crash_bundle(
                stack_hash=0,
                trace="",
                session={},
                extra={
                    "report_source": "f12_manual",
                    "server_request_count": 0,
                    "handshake": {"server_boot_build": 3698585},
                    "manual_report": {
                        "category_id": "UI",
                        "title": "F12 report with sentinel zero build",
                        "description": "Submitted without numeric client build",
                        "client_build": bad_build,
                    },
                },
            )
            build_info = sync_mod._extract_build_info(f12_bundle)
            self.assertIsNone(build_info["client_build"], f"expected None client_build for {bad_build!r}")
            self.assertEqual(build_info["server_build"], "3698585")
            self.assertFalse(build_info["mismatch"], f"unexpected mismatch for {bad_build!r}")
            self.assertIsNone(
                sync_mod.classify_environment_fault(f12_bundle),
                f"non-positive client_build {bad_build!r} misclassified as env fault",
            )
            title, labels = sync_mod.build_issue_title_and_labels(f12_bundle)
            self.assertEqual(title, "[Bug Report][UI] F12 report with sentinel zero build")
            self.assertNotIn("environment", labels)

            # Even with an exception traceback and authenticated session, Build=0 must not trigger build_mismatch.
            exc_bundle = _make_crash_bundle(
                stack_hash=-9100,
                trace=(
                    "Traceback (most recent call last):\n"
                    '  File "eve/client/ui/overview.py", line 44, in Refresh\n'
                    "KeyError: 'solarSystemID'\n"
                ),
                extra={
                    "server_request_count": 4,
                    "handshake": {"client_boot_build": bad_build, "server_boot_build": 3698585},
                },
            )
            self.assertIsNone(sync_mod.classify_environment_fault(exc_bundle))

    def test_gh1358_r3_03_grpc_unavailable_status_without_error_key_is_transport_failure(self) -> None:
        transport_calls = [
            {
                "kind": "quasar",
                "service": "eve_public.character.api.CreateRequest",
                "method": "Send",
                "status": "UNAVAILABLE(Connection refused)",
            },
            {
                "kind": "quasar",
                "service": "publicGateway",
                "method": "Send",
                "status": "UNAVAILABLE",
            },
            {
                "service": "quasar_grpc",
                "method": "Send",
                "status": "UNAVAILABLE(Connection refused)",
            },
        ]
        for idx, call in enumerate(transport_calls):
            self.assertTrue(
                sync_mod._is_client_transport_failure_call(call),
                f"expected transport failure for call={call}",
            )
            bundle = _make_crash_bundle(
                stack_hash=9920 + idx,
                trace=(
                    "Traceback (most recent call last):\n"
                    '  File "char.py", line 10, in Create\n'
                    "RuntimeError: request failed\n"
                ),
                flight_recorder=[call],
                extra={"server_request_count": 5, "gateway_request_count": 0},
            )
            counts = sync_mod._extract_session_request_counts(bundle)
            self.assertEqual(counts["gateway_requests"], 0, f"call={call} miscounted as observed_gateway")
            self.assertFalse(counts["reached_gateway"], f"call={call} falsely set reached_gateway")
            self.assertTrue(
                sync_mod._is_gateway_failure_bundle(bundle),
                f"call={call} not recognized by _is_gateway_failure_bundle",
            )
            fault = sync_mod.classify_environment_fault(bundle)
            self.assertIsNotNone(fault, f"expected zero_gateway_requests for call={call}")
            assert fault is not None
            self.assertEqual(fault[0], "zero_gateway_requests")
            self.assertEqual(fault[1], "env:zero_gateway_requests")

        # Server-recorded non-transport gRPC status (e.g. UNAUTHENTICATED) still counts as reaching gateway.
        server_reached_call = {
            "service": "publicGatewaySvc",
            "method": "SendRequest",
            "status": "UNAUTHENTICATED",
        }
        self.assertFalse(sync_mod._is_client_transport_failure_call(server_reached_call))
        reached_bundle = _make_crash_bundle(
            stack_hash=9930,
            trace="quasar gateway returned UNAUTHENTICATED",
            flight_recorder=[server_reached_call],
            extra={"server_request_count": 5, "gateway_request_count": 0},
        )
        self.assertIsNone(sync_mod.classify_environment_fault(reached_bundle))

    def test_gh1426_macho_protocol_report_is_not_classified_as_zero_gateway_requests(self) -> None:
        macho_bundle = _make_crash_bundle(
            stack_hash=8589232799888905572,
            trace="",
            flight_recorder=[
                {
                    "kind": "macho",
                    "service": "beyonce",
                    "method": "CmdStargateJump",
                    "status": "ERR",
                    "error": "UserError('CustomInfo', {'info': (101, u'UI/GateIcons/GateClosed')})",
                }
            ],
            extra={
                "report_source": "f12_manual",
                "stack_key": "playtester:quasar-macho:protocol_error",
                "server_request_count": 113,
                "gateway_request_count": 0,
                "manual_report": {
                    "category_id": "quasar-macho",
                    "category_name": "protocol_error",
                    "title": "Protocol/Quasar error on jump_gate (beyonce.CmdStargateJump): UserError('CustomInfo', {'info': (101, u'UI/GateIcons/GateClosed')})",
                    "description": '{"status": "ERR", "kind": "macho", "service": "beyonce", "method": "CmdStargateJump"}',
                },
            },
        )
        self.assertFalse(sync_mod._is_gateway_failure_bundle(macho_bundle))
        self.assertIsNone(sync_mod.classify_environment_fault(macho_bundle))
        title, labels = sync_mod.build_issue_title_and_labels(macho_bundle)
        self.assertNotIn("environment", labels)
        self.assertNotIn("[Environment]", title)

    def test_gh1455_benign_client_tracebacks_suppressed_with_negative_cases(self) -> None:
        market_alert_bundle = _make_crash_bundle(
            stack_hash=1448116342,
            stack_key=(
                " \nCaught at:\n"
                "/opt/ccp/buildAgentFull/work/d50fcc72fcb6ee81/eve/release/V24.02/packages/carbonui/uilib.py(708) Update\n"
                " \nThrown at:\n"
                "/opt/ccp/buildAgentFull/work/d50fcc72fcb6ee81/eve/release/V24.02/eve/client/script/ui/shared/market/marketbase.py(3718) _OnResize\n"
            ),
            trace=(
                "EXCEPTION #7 logged at 11/20/2026 05:46:24 : Info: AttributeError: 'NoneType' object has no attribute 'max' \n\n"
                "Formatted exception info: AttributeError: 'NoneType' object has no attribute 'max'\n\n"
                "Thrown at:\n"
                " /opt/ccp/buildAgentFull/work/d50fcc72fcb6ee81/eve/release/V24.02/packages/carbonui/primitives/base.py(2097) UpdateAlignment\n"
                " /opt/ccp/buildAgentFull/work/d50fcc72fcb6ee81/eve/release/V24.02/eve/client/script/ui/shared/market/marketbase.py(3718) _OnResize\n"
                "        absHeight = 555\n"
                "Thread Locals:  session was <Session>\n"
                "Stackhash: 1448116342\n"
                "EXCEPTION END\n"
            ),
            flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
        )
        self.assertTrue(sync_mod.is_benign_client_traceback_bundle(market_alert_bundle))

        minimap_py_trace = (
            "AttributeError: 'MapSlider' object has no attribute 'minimumWidth'\n"
            "log.LogException args=() kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "/opt/ccp/eve/packages/carbonui/graphs/minimap.py", line 100, in UpdateAlignment\n'
            '  File "/opt/ccp/eve/packages/carbonui/graphs/minimap.py", line 111, in _ResizeSlider\n'
            "AttributeError: 'MapSlider' object has no attribute 'minimumWidth'\n\n"
            "CALLSTACK:\n"
            '  File "/opt/ccp/eve/packages/carbonui/uilib.py", line 708, in Update\n'
        )
        minimap_manual_bundle = {
            "report_source": "f12_manual",
            "stack_hash": 2541493741815843466,
            "stack_key": "playtester:market / carbonui.graphs.minimap:client_exception",
            "trace": minimap_py_trace,
            "manual_report": {
                "report_key": "playtester_step57954_23453317bfcfca8a",
                "category_id": "market / carbonui.graphs.minimap",
                "title": "[Bug Report] AttributeError: 'MapSlider' object has no attribute 'minimumWidth'",
            },
            "flight_recorder": [{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
            "session": {"character_id": 90000001},
        }
        self.assertTrue(sync_mod.is_benign_client_traceback_bundle(minimap_manual_bundle))
        self.assertTrue(
            sync_mod.is_benign_client_traceback(
                "<pre>/app/client/packages/carbonui/graphs/minimap.pyj(111) _ResizeSlider</pre>\n"
                "Formatted exception info: AttributeError: 'MapSlider' object has no attribute 'minimumWidth'\n"
                "EXCEPTION END\n",
                "",
            )
        )
        # LogException (non-concrete exception class) may fall back to headline when location matches.
        self.assertTrue(
            sync_mod.is_benign_client_traceback(
                "logmodule.LogException args=()\nNoneType: None\nCALLSTACK:\n  File \"marketbase.py\", line 3718, in _OnResize\n",
                "",
                headline="[Bug Report] AttributeError: 'NoneType' object has no attribute 'max'",
            )
        )

        # Negative cases that must NOT be suppressed.
        negative_cases = [
            _make_crash_bundle(
                stack_hash=101,
                trace='Traceback (most recent call last):\n  File "marketbase.py", line 3718, in _OnResize\nKeyError: "max"',
                flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
            ),
            _make_crash_bundle(
                stack_hash=102,
                trace='Traceback (most recent call last):\n  File "marketbase.py", line 3718, in LoadOrders\nAttributeError: \'NoneType\' object has no attribute \'max\'',
                flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
            ),
            _make_crash_bundle(
                stack_hash=103,
                trace='Traceback (most recent call last):\n  File "other_marketbase.py", line 3718, in _OnResize\nAttributeError: \'NoneType\' object has no attribute \'max\'',
                flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
            ),
            _make_crash_bundle(
                stack_hash=104,
                trace=(
                    "Traceback (most recent call last):\n"
                    '  File "marketbase.py", line 3718, in _OnResize\n'
                    '  File "market_orders.py", line 99, in ApplyFilter\n'
                    "AttributeError: 'NoneType' object has no attribute 'max'\n"
                ),
                flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
            ),
            _make_crash_bundle(
                stack_hash=105,
                trace=(
                    "Traceback (most recent call last):\n"
                    '  File "marketbase.py", line 3718, in _OnResize\n'
                    "RuntimeError: ServerException: 'NoneType' object has no attribute 'max'\n"
                ),
                flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
            ),
            {
                "report_source": "f12_manual",
                "stack_hash": 106,
                "stack_key": "f12:ui:Market broken",
                "trace": "",
                "manual_report": {
                    "report_key": "EBR-9999-ABCDEF",
                    "category_id": "UI",
                    "title": "Market window unresponsive",
                    "description": "Buttons stopped responding.",
                    "client_logs_tail": [
                        '  File "minimap.py", line 111, in _ResizeSlider',
                        "AttributeError: 'MapSlider' object has no attribute 'minimumWidth'",
                    ],
                },
                "session": {"character_id": 90000001},
            },
            # Downstream `<lambda>` frame under `marketbase._OnResize` must not be suppressed.
            _make_crash_bundle(
                stack_hash=107,
                trace=(
                    "Traceback (most recent call last):\n"
                    '  File "marketbase.py", line 3718, in _OnResize\n'
                    '  File "marketbase.py", line 3724, in <lambda>\n'
                    "AttributeError: 'NoneType' object has no attribute 'max'\n"
                ),
                flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
            ),
            # Real non-matching leaf frame failing `_LOCATION_FUNC_RE` must not fall back to `stack_key`.
            _make_crash_bundle(
                stack_hash=108,
                stack_key="/opt/ccp/eve/marketbase.py(3718) _OnResize",
                trace=(
                    "Traceback (most recent call last):\n"
                    '  File "/opt/ccp/eve/marketbase.py", line 3718, in _OnResize\n'
                    '  File "/opt/ccp/eve/my window.py", line 88, in OnOpen\n'
                    "AttributeError: 'NoneType' object has no attribute 'max'\n"
                ),
                flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
            ),
            _make_crash_bundle(
                stack_hash=109,
                stack_key="/opt/ccp/eve/marketbase.py(3718) _OnResize",
                trace=(
                    "Thrown at:\n"
                    "<pre>/opt/ccp/eve/other.py(42)</pre>\n"
                    "Formatted exception info: AttributeError: 'NoneType' object has no attribute 'max'\n"
                    "EXCEPTION END\n"
                ),
                flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
            ),
            # Different AttributeError with benign substring in `Thread Locals:` / `stack_key` / post-`EXCEPTION END`.
            _make_crash_bundle(
                stack_hash=110,
                stack_key="AttributeError: 'NoneType' object has no attribute 'max'",
                trace=(
                    "Formatted exception info: AttributeError: 'NoneType' object has no attribute 'buyOrders'\n"
                    "Thrown at:\n"
                    " /opt/ccp/eve/marketbase.py(3718) _OnResize\n"
                    "Thread Locals: prev = \"'NoneType' object has no attribute 'max'\"\n"
                    "EXCEPTION END\n"
                    "AttributeError: 'NoneType' object has no attribute 'max'\n"
                ),
                flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
            ),
            # Concrete non-AttributeError must not be overridden by headline fallback or locals.
            {
                "report_source": "f12_manual",
                "stack_hash": 111,
                "stack_key": "marketbase.py(3718) _OnResize",
                "trace": (
                    "Formatted exception info: KeyError: 'price'\n"
                    "Thrown at:\n"
                    " /opt/ccp/eve/marketbase.py(3718) _OnResize\n"
                    "Thread Locals: err = \"AttributeError: 'NoneType' object has no attribute 'max'\"\n"
                    "EXCEPTION END\n"
                ),
                "manual_report": {
                    "report_key": "playtester_step57955_1111222233334444",
                    "category_id": "UI",
                    "title": "[Bug Report] AttributeError: 'NoneType' object has no attribute 'max'",
                },
                "session": {"character_id": 90000001},
            },
            # Prose path inside exception message must not be misparsed as frame.
            _make_crash_bundle(
                stack_hash=112,
                trace=(
                    "Traceback (most recent call last):\n"
                    '  File "other.py", line 10, in Foo\n'
                    "RuntimeError: failed in /app/marketbase.py(3718) _OnResize\n"
                ),
                flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
            ),
        ]
        for idx, neg in enumerate(negative_cases):
            self.assertFalse(sync_mod.is_benign_client_traceback_bundle(neg), f"negative case {idx} suppressed")

        # Direct `headline` fallback must not suppress a concrete KeyError or a different AttributeError.
        self.assertFalse(
            sync_mod.is_benign_client_traceback(
                'Traceback (most recent call last):\n  File "marketbase.py", line 3718, in _OnResize\nKeyError: "price"\n',
                "",
                headline="AttributeError: 'NoneType' object has no attribute 'max'",
            )
        )
        self.assertFalse(
            sync_mod.is_benign_client_traceback(
                'Traceback (most recent call last):\n  File "marketbase.py", line 3718, in _OnResize\nAttributeError: \'NoneType\' object has no attribute \'buyOrders\'\n',
                "'NoneType' object has no attribute 'max'",
                headline="AttributeError: 'NoneType' object has no attribute 'max'",
            )
        )

        # Direct create_or_update_issue suppresses benign bundle with zero gh calls.
        fake_gh = FakeGitHubCLI()
        with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
            sync_mod.subprocess, "run", side_effect=fake_gh
        ):
            self.assertIsNone(sync_mod.create_or_update_issue("saverchenkov/EvoMu", market_alert_bundle, dry_run=False))
        self.assertEqual(len(fake_gh.created_issues), 0)
        self.assertEqual(len(fake_gh.comments), 0)

        # main() archives benign bundles + attachments while still filing the single genuine crash.
        with tempfile.TemporaryDirectory() as tmpdir:
            qdir = Path(tmpdir) / "queue"
            udir = Path(tmpdir) / "uploaded"
            qdir.mkdir()
            att_dir = qdir / "attachments" / "playtester_step57954_23453317bfcfca8a"
            att_dir.mkdir(parents=True)
            (att_dir / "shot.png").write_bytes(b"png")
            (qdir / "01_market_benign.json").write_text(json.dumps(market_alert_bundle), encoding="utf-8")
            (qdir / "02_minimap_benign.json").write_text(json.dumps(minimap_manual_bundle), encoding="utf-8")
            (qdir / "03_genuine.json").write_text(json.dumps(negative_cases[0]), encoding="utf-8")

            fake_batch_gh = FakeGitHubCLI()
            buf = io.StringIO()
            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=fake_batch_gh
            ), contextlib.redirect_stdout(buf):
                rc = sync_mod.main(["--queue-dir", str(qdir), "--uploaded-dir", str(udir)])
            out = buf.getvalue()
            self.assertEqual(rc, 0)
            self.assertEqual(len(fake_batch_gh.created_issues), 1)
            self.assertIn("KeyError", fake_batch_gh.created_issues[0]["title"])
            self.assertIn("Processed 3/3 incident file(s).", out)
            self.assertEqual(list(qdir.glob("*.json")), [])
            self.assertTrue((udir / "01_market_benign.json").exists())
            self.assertTrue((udir / "02_minimap_benign.json").exists())
            self.assertTrue((udir / "03_genuine.json").exists())
            self.assertTrue((udir / "attachments" / "playtester_step57954_23453317bfcfca8a" / "shot.png").exists())

    def test_gh1455_hash_alignment_deduplicates_against_legacy_client_hash_1314(self) -> None:
        market_1314_trace = (
            "EXCEPTION #7 logged at 11/20/2026 05:46:24 : Info: AttributeError: 'NoneType' object has no attribute 'max' \n\n"
            "Formatted exception info: AttributeError: 'NoneType' object has no attribute 'max'\n\n"
            "Thrown at:\n"
            " /opt/ccp/eve/eve/client/script/ui/shared/market/marketbase.py(3718) _OnResize\n"
            "EXCEPTION END\n"
        )
        market_1314_bundle = _make_crash_bundle(
            stack_hash=1448116342,  # 0x0000000056508076
            stack_key="marketbase.py(3718) _OnResize",
            trace=market_1314_trace,
            flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
        )
        canonical_market_hash = sync_mod._effective_bundle_hash(market_1314_bundle)
        self.assertEqual(canonical_market_hash & 0xFFFFFFFFFFFFFFFF, 0xB5206B0747A727CE)
        # Canonicalization must NOT destroy raw client `stack_hash` in the bundle dict.
        self.assertEqual(market_1314_bundle["stack_hash"], 1448116342)
        self.assertEqual(
            sync_mod._legacy_client_stack_hashes(market_1314_bundle, effective_hash=canonical_market_hash),
            [1448116342],
        )

        # Non-benign bundle with legacy `#1314` client hash format (`0x0000000056508076` in GitHub issue body)
        # must still resolve to that existing closed issue rather than opening a duplicate.
        non_benign_client_bundle = _make_crash_bundle(
            stack_hash=1448116342,  # 0x0000000056508076
            stack_key="marketbase.py(3725) RefreshOrders",
            trace=(
                "Formatted exception info:\n"
                "KeyError: 'orders'\n\n"
                "Thrown at:\n"
                "/app/eve/client/script/ui/shared/market/marketbase.py(3725) RefreshOrders\n"
                "EXCEPTION END\n"
            ),
            flight_recorder=[{"service": "marketProxy", "method": "GetOrders", "status": "OK", "duration_micros": 100}],
        )
        fake_gh = FakeGitHubCLI()
        fake_gh.created_issues.append(
            {
                "number": 1314,
                "title": "[Client Crash] KeyError: 'orders'",
                "labels": ["bug", "client-crash"],
                "body": "**Signature Hash:** `0x0000000056508076` (or `1448116342`) | **Occurrences Captured:** `1`",
                "state": "CLOSED",
                "url": "https://github.com/saverchenkov/EvoMu/issues/1314",
            }
        )
        with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
            sync_mod.subprocess, "run", side_effect=fake_gh
        ):
            issue_id = sync_mod.create_or_update_issue(
                "saverchenkov/EvoMu",
                non_benign_client_bundle,
                occurrences=1,
                dry_run=False,
            )
        self.assertEqual(issue_id, 1314)
        self.assertEqual(len(fake_gh.created_issues), 1)
        self.assertEqual(fake_gh.reopened, [1314])
        self.assertEqual(len(fake_gh.comments), 1)

        # When creating a fresh canonical issue, preserve Client Stack Hash in markdown while avoiding
        # false collisions across unrelated bugs that share a synthetic client hash.
        fresh_gh = FakeGitHubCLI()
        bundle_x = _make_crash_bundle(
            stack_hash=1448116342,
            trace='Traceback (most recent call last):\n  File "alpha.py", line 10, in Foo\nValueError: alpha failed\n',
            flight_recorder=[{"service": "alpha", "method": "Get", "status": "OK", "duration_micros": 100}],
        )
        bundle_y = _make_crash_bundle(
            stack_hash=1448116342,
            trace='Traceback (most recent call last):\n  File "beta.py", line 20, in Bar\nTypeError: beta failed\n',
            flight_recorder=[{"service": "beta", "method": "Get", "status": "OK", "duration_micros": 100}],
        )
        with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
            sync_mod.subprocess, "run", side_effect=fresh_gh
        ):
            id_x = sync_mod.create_or_update_issue("saverchenkov/EvoMu", bundle_x, dry_run=False)
            id_y = sync_mod.create_or_update_issue("saverchenkov/EvoMu", bundle_y, dry_run=False)
        self.assertNotEqual(id_x, id_y)
        self.assertEqual(len(fresh_gh.created_issues), 2)
        self.assertIn("**Client Stack Hash:** `0x0000000056508076`", fresh_gh.created_issues[0]["body"])

    def test_tt08_extra_bundle_preserved_when_post_comment_fails_under_archive_and_delete(self) -> None:
        tb_primary = (
            'Traceback (most recent call last):\n'
            '  File "eve/client/ui/mod_a.py", line 10, in Open\n'
            'RuntimeError: primary crash\n'
        )
        tb_extra_ok = (
            'Traceback (most recent call last):\n'
            '  File "eve/client/ui/mod_b.py", line 20, in Open\n'
            'RuntimeError: extra comment ok\n'
        )
        tb_extra_fail = (
            'Traceback (most recent call last):\n'
            '  File "eve/client/ui/mod_c.py", line 30, in Open\n'
            'RuntimeError: extra comment fail\n'
        )

        for delete_mode in (False, True):
            with self.subTest(delete=delete_mode), tempfile.TemporaryDirectory() as td:
                root = Path(td)
                qdir = root / "queue"
                udir = root / "uploaded"
                qdir.mkdir(parents=True)
                attach_dir = qdir / "attachments" / "playtester_step999_extra_fail"
                attach_dir.mkdir(parents=True)
                (attach_dir / "screenshot.png").write_bytes(b"PNG")

                (qdir / "01_primary.json").write_text(
                    json.dumps(_make_crash_bundle(stack_hash=-9101, trace=tb_primary, session={}, extra={"server_request_count": 0})),
                    encoding="utf-8",
                )
                (qdir / "02_extra_ok.json").write_text(
                    json.dumps(_make_crash_bundle(stack_hash=-9102, trace=tb_extra_ok, session={}, extra={"server_request_count": 0})),
                    encoding="utf-8",
                )
                extra_fail_bundle = _make_crash_bundle(
                    stack_hash=-9103,
                    trace=tb_extra_fail,
                    session={},
                    extra={
                        "server_request_count": 0,
                        "manual_report": {"report_key": "playtester_step999_extra_fail", "category_id": "client-python"},
                    },
                )
                (qdir / "03_extra_fail.json").write_text(json.dumps(extra_fail_bundle), encoding="utf-8")

                comment_calls = {"n": 0}
                fake_gh = FakeGitHubCLI()

                def _gh_comment_fails_on_second(cmd, **kwargs):  # type: ignore[no-untyped-def]
                    if cmd[:3] == ["gh", "issue", "comment"]:
                        comment_calls["n"] += 1
                        if comment_calls["n"] == 2:
                            raise subprocess.CalledProcessError(1, cmd, stderr="HTTP 502 rate limited")
                    return fake_gh(cmd, **kwargs)

                argv = ["--queue-dir", str(qdir), "--uploaded-dir", str(udir)]
                if delete_mode:
                    argv.append("--delete")
                with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                    sync_mod.subprocess, "run", side_effect=_gh_comment_fails_on_second
                ):
                    rc = sync_mod.main(argv)

                self.assertEqual(rc, 1)
                self.assertEqual(len(fake_gh.created_issues), 1)
                self.assertEqual(len(fake_gh.comments), 1)
                remaining = sorted(p.name for p in qdir.glob("*.json"))
                self.assertEqual(remaining, ["03_extra_fail.json"])
                self.assertTrue((attach_dir / "screenshot.png").exists())
                if not delete_mode:
                    archived = sorted(p.name for p in udir.glob("*.json"))
                    self.assertEqual(archived, ["01_primary.json", "02_extra_ok.json"])
                else:
                    self.assertFalse((udir / "01_primary.json").exists())
                    self.assertFalse((udir / "02_extra_ok.json").exists())

    def test_tt18_find_existing_issue_fails_closed_on_gh_search_errors(self) -> None:
        bundle = _make_crash_bundle(
            stack_hash=-9201,
            trace='Traceback (most recent call last):\n  File "ship.py", line 12, in Board\nKeyError: 42\n',
            flight_recorder=[{"service": "ship", "method": "Board", "status": "OK", "duration_micros": 100}],
        )
        created_called = {"n": 0}

        def _failing_search(cmd, **kwargs):  # type: ignore[no-untyped-def]
            if cmd[:3] == ["gh", "issue", "list"]:
                raise subprocess.CalledProcessError(1, cmd, stderr="API rate limit exceeded")
            if cmd[:3] == ["gh", "issue", "create"]:
                created_called["n"] += 1
            return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")

        with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
            sync_mod.subprocess, "run", side_effect=_failing_search
        ):
            with self.assertRaises(sync_mod.IssueLookupError):
                sync_mod.find_existing_issue("saverchenkov/EvoMu", -9201, dry_run=False)
            with self.assertRaises(sync_mod.IssueLookupError):
                sync_mod.create_or_update_issue("saverchenkov/EvoMu", bundle, dry_run=False)

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            udir = root / "uploaded"
            qdir.mkdir(parents=True)
            (qdir / "bundle.json").write_text(json.dumps(bundle), encoding="utf-8")
            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=_failing_search
            ):
                rc = sync_mod.main(["--queue-dir", str(qdir), "--uploaded-dir", str(udir)])
            self.assertEqual(rc, 1)
            self.assertEqual([p.name for p in qdir.glob("*.json")], ["bundle.json"])
        self.assertEqual(created_called["n"], 0)

    def test_tt19_persistent_cache_dedups_through_github_search_indexing_lag_and_updates_closed_state(self) -> None:
        tb = (
            'Traceback (most recent call last):\n'
            '  File "eve/client/script/ui/fitting.py", line 88, in SwapShip\n'
            'KeyError: 1897898652\n'
        )
        auto_bundle = _make_crash_bundle(
            stack_hash=-9301,
            trace=tb,
            flight_recorder=[{"service": "ship", "method": "Board", "status": "OK", "duration_micros": 100}],
        )
        manual_bundle = _make_crash_bundle(
            stack_hash=-9302,
            stack_key="playtester:client-python:client_traceback",
            trace=tb,
            flight_recorder=[{"service": "dogmaIM", "method": "GetAllInfo", "status": "OK", "duration_micros": 100}],
            extra={
                "report_source": "f12_manual",
                "manual_report": {"category_id": "client-python", "title": "KeyError: 1897898652"},
            },
        )
        self.assertEqual(sync_mod._effective_bundle_hash(auto_bundle), sync_mod._effective_bundle_hash(manual_bundle))

        class _LaggingSearchGH(FakeGitHubCLI):
            def __init__(self) -> None:
                super().__init__()
                self._next_num = 1409
                self.lag_search = True
                self.override_state: dict[int, str] = {}

            def __call__(self, cmd, **kwargs):  # type: ignore[no-untyped-def]
                if cmd[:3] == ["gh", "issue", "list"]:
                    if self.lag_search:
                        return subprocess.CompletedProcess(cmd, 0, stdout="[]", stderr="")
                    res = super().__call__(cmd, **kwargs)
                    rows = json.loads(res.stdout or "[]")
                    for r in rows:
                        if r["number"] in self.override_state:
                            r["state"] = self.override_state[r["number"]]
                    return subprocess.CompletedProcess(cmd, 0, stdout=json.dumps(rows), stderr="")
                return super().__call__(cmd, **kwargs)

        lag_gh = _LaggingSearchGH()
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            udir = root / "uploaded"
            qdir.mkdir(parents=True)

            # Run 1: creates #1410 and persists hash -> #1410 to local issue_cache.json
            (qdir / "01_auto.json").write_text(json.dumps(auto_bundle), encoding="utf-8")
            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=lag_gh
            ):
                rc1 = sync_mod.main(["--queue-dir", str(qdir), "--uploaded-dir", str(udir)])
            self.assertEqual(rc1, 0)
            self.assertEqual(len(lag_gh.created_issues), 1)
            self.assertTrue((root / "issue_cache.json").exists())

            # Run 2 (1s later while GitHub Search still returns []): must comment on #1410, NOT create #1411!
            (qdir / "02_manual.json").write_text(json.dumps(manual_bundle), encoding="utf-8")
            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=lag_gh
            ):
                rc2 = sync_mod.main(["--queue-dir", str(qdir), "--uploaded-dir", str(udir)])
            self.assertEqual(rc2, 0)
            self.assertEqual(len(lag_gh.created_issues), 1)
            self.assertEqual(len(lag_gh.comments), 1)
            self.assertEqual(lag_gh.comments[0]["number"], 1410)

            # Run 3: once GitHub indexes #1410 and reports it CLOSED, live CLOSED state overrides stale OPEN cache & reopens #1410.
            lag_gh.lag_search = False
            lag_gh.override_state[1410] = "CLOSED"
            (qdir / "03_retest.json").write_text(json.dumps(auto_bundle), encoding="utf-8")
            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=lag_gh
            ):
                rc3 = sync_mod.main(["--queue-dir", str(qdir), "--uploaded-dir", str(udir)])
            self.assertEqual(rc3, 0)
            self.assertEqual(lag_gh.reopened, [1410])
            self.assertEqual(len(lag_gh.comments), 2)

    def test_tt20_prefer_open_over_closed_legacy_hashes_no_literal_1_and_1439_non_collapse(self) -> None:
        # 1. Prefer OPEN over CLOSED even when CLOSED comes first or matches primary while OPEN matches legacy/later row.
        primary_h = -777001
        alias_h = 888002
        closed_then_open = [
            {"number": 1314, "title": "Old closed", "state": "CLOSED"},
            {"number": 1505, "title": "Active open", "state": "OPEN"},
        ]

        def _gh_multi_state(cmd, **kwargs):  # type: ignore[no-untyped-def]
            term = cmd[cmd.index("--search") + 1]
            if f"(or `{alias_h}`)" in term:
                return subprocess.CompletedProcess(cmd, 0, stdout=json.dumps(closed_then_open), stderr="")
            if f"(or `{primary_h}`)" in term:
                return subprocess.CompletedProcess(
                    cmd, 0, stdout=json.dumps([{"number": 1200, "title": "Closed primary", "state": "CLOSED"}]), stderr=""
                )
            return subprocess.CompletedProcess(cmd, 0, stdout="[]", stderr="")

        with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
            sync_mod.subprocess, "run", side_effect=_gh_multi_state
        ):
            found = sync_mod.find_existing_issue("saverchenkov/EvoMu", primary_h, alias_hashes=[alias_h])
        self.assertEqual(found, (1505, "OPEN"))

        # 2. Unparsed `gh issue create` output must raise (never return literal `1`).
        bundle = _make_crash_bundle(
            stack_hash=-9401,
            trace='Traceback (most recent call last):\n  File "foo.py", line 9, in Run\nValueError: bad url test\n',
            flight_recorder=[{"service": "charMgr", "method": "GetInfo", "status": "OK", "duration_micros": 50}],
        )

        def _gh_bad_create_url(cmd, **kwargs):  # type: ignore[no-untyped-def]
            if cmd[:3] == ["gh", "label", "create"]:
                return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")
            if cmd[:3] == ["gh", "issue", "list"]:
                return subprocess.CompletedProcess(cmd, 0, stdout="[]", stderr="")
            if cmd[:3] == ["gh", "issue", "create"]:
                return subprocess.CompletedProcess(cmd, 0, stdout="Created successfully (no URL)", stderr="")
            raise AssertionError(f"Should not post comment or reopen: {cmd}")

        with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
            sync_mod.subprocess, "run", side_effect=_gh_bad_create_url
        ):
            with self.assertRaises(RuntimeError):
                sync_mod.create_or_update_issue("saverchenkov/EvoMu", bundle, dry_run=False)

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            udir = root / "uploaded"
            qdir.mkdir(parents=True)
            (qdir / "01_bad_url.json").write_text(json.dumps(bundle), encoding="utf-8")
            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=_gh_bad_create_url
            ):
                rc = sync_mod.main(["--queue-dir", str(qdir), "--uploaded-dir", str(udir)])
            self.assertEqual(rc, 1)
            self.assertEqual([p.name for p in qdir.glob("*.json")], ["01_bad_url.json"])

        # 3. Authenticated world session (even with 0 step requests & no char_id) must NOT collapse onto env:zero_server_requests (#1439/#1369).
        autopilot_1439_bundle = _make_crash_bundle(
            stack_hash=-9402,
            trace='File "eve/client/script/parklife/autopilot.py", line 559, in AP_ApproachDestID\nUserError: ShipAlreadyInWarp',
            session={"solar_system_id": 30002801},
            extra={"server_request_count": 0},
        )
        self.assertIsNone(sync_mod.classify_environment_fault(autopilot_1439_bundle))
        self.assertNotEqual(
            sync_mod._effective_bundle_hash(autopilot_1439_bundle),
            sync_mod._fnv1a_i64("env:zero_server_requests"),
        )

    def test_bf1483a_01_and_02_shared_raw_hash_cache_isolation_and_non_fcntl_locks(self) -> None:
        # 1. Shared raw client stack_hash must NOT poison persistent issue_cache.json across distinct crashes.
        bundle_a = _make_crash_bundle(
            stack_hash=1448116342,
            trace='Traceback (most recent call last):\n  File "alpha_shared.py", line 10, in Foo\nValueError: alpha failed\n',
            flight_recorder=[{"service": "alpha", "method": "Get", "status": "OK", "duration_micros": 100}],
        )
        bundle_b = _make_crash_bundle(
            stack_hash=1448116342,
            trace='Traceback (most recent call last):\n  File "beta_shared.py", line 20, in Bar\nTypeError: beta failed\n',
            flight_recorder=[{"service": "beta", "method": "Get", "status": "OK", "duration_micros": 100}],
        )
        fake_gh = FakeGitHubCLI()
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            udir = root / "uploaded"
            qdir.mkdir(parents=True)
            (qdir / "01_a.json").write_text(json.dumps(bundle_a), encoding="utf-8")
            (qdir / "02_b.json").write_text(json.dumps(bundle_b), encoding="utf-8")
            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=fake_gh
            ):
                rc = sync_mod.main(["--queue-dir", str(qdir), "--uploaded-dir", str(udir)])
            self.assertEqual(rc, 0)
            self.assertEqual(len(fake_gh.created_issues), 2)
            self.assertEqual(len(fake_gh.comments), 0)
            cache_data = json.loads((root / "issue_cache.json").read_text(encoding="utf-8"))
            repo_cache = cache_data["repos"]["saverchenkov/EvoMu"]
            raw_key = sync_mod._issue_cache_key(1448116342)
            self.assertNotIn(raw_key, repo_cache)
            self.assertIn(sync_mod._issue_cache_key(sync_mod._effective_bundle_hash(bundle_a)), repo_cache)
            self.assertIn(sync_mod._issue_cache_key(sync_mod._effective_bundle_hash(bundle_b)), repo_cache)

            # Even when --delete + custom queue dir is used without --uploaded-dir, cache lives next to queue dir.
            custom_q = root / "custom_q"
            custom_q.mkdir(parents=True)
            (custom_q / "03_a.json").write_text(json.dumps(bundle_a), encoding="utf-8")
            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=fake_gh
            ):
                rc_del = sync_mod.main(["--delete", "--queue-dir", str(custom_q)])
            self.assertEqual(rc_del, 0)
            self.assertTrue((root / ".custom_q_issue_cache.json").exists())

        # 2. check_gh_installed() == False fails closed with IssueLookupError.
        with mock.patch.object(sync_mod, "check_gh_installed", return_value=False):
            with self.assertRaises(sync_mod.IssueLookupError):
                sync_mod.find_existing_issue("saverchenkov/EvoMu", -9500, dry_run=False)

        # 3. _sync_lock works without fcntl via msvcrt and via O_CREAT|O_EXCL fallback.
        with tempfile.TemporaryDirectory() as td:
            lock_p = Path(td) / "sync.lock"
            fake_msvcrt = mock.Mock()
            fake_msvcrt.LK_LOCK = 1
            fake_msvcrt.LK_UNLCK = 0
            with mock.patch.object(sync_mod, "fcntl", None), mock.patch.object(sync_mod, "msvcrt", fake_msvcrt):
                with sync_mod._sync_lock(lock_p):
                    self.assertTrue(lock_p.exists())
            self.assertEqual(fake_msvcrt.locking.call_count, 2)
            self.assertEqual(fake_msvcrt.locking.call_args_list[0].args[1:], (1, 1))
            self.assertEqual(fake_msvcrt.locking.call_args_list[1].args[1:], (0, 1))

            excl_lock = Path(td) / "excl.lock"
            with mock.patch.object(sync_mod, "fcntl", None), mock.patch.object(sync_mod, "msvcrt", None):
                with sync_mod._sync_lock(excl_lock):
                    self.assertTrue(excl_lock.exists())
            self.assertFalse(excl_lock.exists())

    def test_dedup_same_step_macho_and_client_python_queue_bundles(self) -> None:
        trace_py = (
            'Traceback (most recent call last):\n'
            '  File "packages/fitting/ui/fittingSlotController.py", line 91, in FitModule\n'
            "UserError: User error, msg=ModuleFitFailed, dict={'reason': 'Low CPU'}\n"
        )
        client_bundle = {
            "fingerprint": "be997965964ffe5b",
            "stack_hash": 111111111,
            "stack_key": "playtester:client-python:client_traceback",
            "report_source": "f12_manual",
            "step": 96364,
            "git_head": "3c6b4a75c",
            "kind": "client_traceback",
            "subsystem": "client-python",
            "summary": "User error, msg=ModuleFitFailed\nLocation: fittingSlotController.py:91",
            "trace": trace_py,
            "traceback": trace_py,
            "manual_report": {
                "report_key": "playtester_step96364_be997965964ffe5b",
                "category_id": "client-python",
                "category_name": "client_traceback",
                "title": "Client exception: User error, msg=ModuleFitFailed",
                "description": "User error, msg=ModuleFitFailed",
            },
            "flight_recorder": [],
        }
        macho_summary = json.dumps(
            {
                "service": "dogmaIM",
                "method": "Fit",
                "error": "UserError('ModuleFitFailed', {'reason': 'Low CPU'})",
            }
        )
        macho_bundle = {
            "fingerprint": "fc0c0ad7d60d499e",
            "stack_hash": 222222222,
            "stack_key": "playtester:macho:protocol_error",
            "report_source": "f12_manual",
            "step": 96364,
            "git_head": "3c6b4a75c",
            "kind": "protocol_error",
            "subsystem": "macho",
            "summary": macho_summary,
            "trace": "",
            "traceback": "",
            "manual_report": {
                "report_key": "playtester_step96364_fc0c0ad7d60d499e",
                "category_id": "macho",
                "category_name": "protocol_error",
                "title": "MachoNet error on click_widget (dogmaIM.Fit): UserError('ModuleFitFailed')",
                "description": macho_summary,
            },
            "flight_recorder": [
                {
                    "offset_ms": -8,
                    "service": "dogmaIM",
                    "method": "Fit",
                    "args_summary": "(1001, 2002)",
                    "status": "UserError('ModuleFitFailed')",
                    "duration_micros": 3100,
                }
            ],
        }

        # Different step must NOT absorb.
        other_step_client = dict(client_bundle, step=96365)
        kept_diff, abs_diff = sync_mod._dedup_same_step_macho_client_bundles(
            [("a.json", other_step_client), ("b.json", dict(macho_bundle))]
        )
        self.assertEqual(len(kept_diff), 2)
        self.assertEqual(abs_diff, {})

        # Mismatched UserError key on same step must NOT absorb.
        other_key_client = dict(
            client_bundle,
            trace='File "x.py", line 1\nUserError: User error, msg=CannotFitStackedModule',
            traceback='File "x.py", line 1\nUserError: User error, msg=CannotFitStackedModule',
        )
        kept_mismatch, abs_mismatch = sync_mod._dedup_same_step_macho_client_bundles(
            [("a.json", other_key_client), ("b.json", dict(macho_bundle))]
        )
        self.assertEqual(len(kept_mismatch), 2)
        self.assertEqual(abs_mismatch, {})

        # Real matching pair (even with client filename sorting before macho): 1 issue + both files archived.
        # Also verify BF-01: pre-existing `[Environment] Zero server requests` issue IS discoverable via
        # find_existing_issue, yet `main()` must NOT collide onto it via alias hash.
        env_zero_hash = sync_mod._fnv1a_i64("env:zero_server_requests")
        fake_gh = FakeGitHubCLI()
        fake_gh.created_issues.append(
            {
                "number": 999,
                "title": "[Environment] Zero server requests reached server",
                "labels": ["environment"],
                "body": (
                    f"**Signature Hash:** `0x{(env_zero_hash & 0xFFFFFFFFFFFFFFFF):016x}` "
                    f"(or `{env_zero_hash}`)\n"
                ),
                "state": "OPEN",
                "url": "https://github.com/saverchenkov/EvoMu/issues/999",
            }
        )
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            udir = root / "uploaded"
            qdir.mkdir(parents=True)
            c_name = "playtester_step96364_be997965964ffe5b.json"
            m_name = "playtester_step96364_fc0c0ad7d60d499e.json"
            (qdir / c_name).write_text(json.dumps(client_bundle), encoding="utf-8")
            (qdir / m_name).write_text(json.dumps(macho_bundle), encoding="utf-8")

            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=fake_gh
            ):
                self.assertEqual(
                    sync_mod.find_existing_issue("saverchenkov/EvoMu", env_zero_hash),
                    (999, "OPEN"),
                )
                rc = sync_mod.main(["--queue-dir", str(qdir), "--uploaded-dir", str(udir), "--cache-file", ""])
            self.assertEqual(rc, 0)
            self.assertEqual(len(fake_gh.created_issues), 2)
            self.assertEqual(len(fake_gh.comments), 0)
            issue = fake_gh.created_issues[-1]
            self.assertIn("[macho]", issue["title"])
            self.assertIn("**Occurrences Captured:** `1`", issue["body"])
            self.assertIn("dogmaIM.Fit", issue["body"])
            self.assertIn("MachoNet RPC Flight Recorder", issue["body"])
            self.assertIn("### 🐍 Client Python Traceback", issue["body"])
            self.assertIn("fittingSlotController.py", issue["body"])
            self.assertNotIn(str(env_zero_hash), issue["body"])
            self.assertEqual(list(qdir.glob("*.json")), [])
            self.assertTrue((udir / c_name).exists())
            self.assertTrue((udir / m_name).exists())

        # Non-env client bundle records its effective alias hash on the absorbed macho bundle.
        non_env_client = dict(
            client_bundle,
            flight_recorder=[
                {
                    "offset_ms": -5,
                    "service": "dogmaIM",
                    "method": "Fit",
                    "args_summary": "(1001, 2002)",
                    "status": "UserError('ModuleFitFailed')",
                    "duration_micros": 1200,
                }
            ],
        )
        kept_non_env, _ = sync_mod._dedup_same_step_macho_client_bundles(
            [("a.json", non_env_client), ("b.json", dict(macho_bundle))]
        )
        self.assertEqual(len(kept_non_env), 1)
        self.assertIn(sync_mod._effective_bundle_hash(non_env_client), kept_non_env[0][1]["alias_hashes"])
        self.assertIn(111111111, kept_non_env[0][1]["alias_hashes"])

        # BF-02: Non-UserError client traceback (KeyError) mentioning UserError in Local variables / frame must NOT absorb.
        keyerr_client_trace = (
            'Traceback (most recent call last):\n'
            '  File "packages/fitting/ui/fittingSlotController.py", line 91, in FitModule\n'
            "    raise UserError('ModuleFitFailed')\n"
            "KeyError: 'shipID'\n"
            "Local variables:\n"
            "  msg = \"User error, msg=ModuleFitFailed\"\n"
            "EXCEPTION END\n"
        )
        keyerr_client = dict(
            client_bundle,
            summary="Client exception: KeyError: 'shipID' (UserError('ModuleFitFailed'))",
            trace=keyerr_client_trace,
            traceback=keyerr_client_trace,
            manual_report={
                "report_key": "playtester_step96364_deadbeef",
                "category_id": "client-python",
                "category_name": "client_traceback",
                "title": "Client exception: KeyError: UserError('ModuleFitFailed')",
                "description": keyerr_client_trace,
            },
        )
        kept_keyerr, abs_keyerr = sync_mod._dedup_same_step_macho_client_bundles(
            [("a.json", keyerr_client), ("b.json", dict(macho_bundle))]
        )
        self.assertEqual(len(kept_keyerr), 2)
        self.assertEqual(abs_keyerr, {})

        # P2 #2: Non-manual bundles must not match _is_macho_protocol_bundle.
        non_manual_macho = {
            "report_source": "exception",
            "subsystem": "macho",
            "kind": "protocol_error",
            "stack_key": "playtester:macho:protocol_error",
            "step": 96364,
        }
        self.assertFalse(sync_mod._is_macho_protocol_bundle(non_manual_macho))

        # Dual-tagged manual macho bundle with client_traceback kind must not self-match as client candidate.
        dual_tagged_macho = dict(macho_bundle, kind="client_traceback")
        kept_dual, abs_dual = sync_mod._dedup_same_step_macho_client_bundles([("m.json", dual_tagged_macho)])
        self.assertEqual(len(kept_dual), 1)
        self.assertEqual(abs_dual, {})

    def test_extract_client_user_error_key_custom_info_preserves_subkey_past_error_name(self) -> None:
        self.assertEqual(
            sync_mod.extract_client_user_error_key(
                "User error, msg=CustomInfo, dict={'info': 'FittingError: no slots'}"
            ),
            "CustomInfo:FittingError",
        )
        self.assertEqual(
            sync_mod.extract_client_user_error_key(
                "UserError('CustomInfo', {'info': 'ValidationError: slot full'})"
            ),
            "CustomInfo:ValidationError",
        )
        self.assertIsNone(
            sync_mod.extract_client_user_error_key(
                "KeyError: \"User error, msg=CustomInfo, dict={'info': 'FittingError: no slots'}\""
            )
        )

    def test_f12_manual_semantic_dedup_across_search_lag_cache(self) -> None:
        # Simulates #1591 + #1592 arriving 44s apart with rephrased titles before GitHub Search indexes #1591.
        b_1591 = {
            "report_source": "f12_manual",
            "fingerprint": "3e2bcddccc66ef00",
            "stack_hash": 4479900602555887360,
            "stack_key": "playtester:contracts:ui_state_desync",
            "trace": "",
            "kind": "ui_state_desync",
            "subsystem": "contracts",
            "manual_report": {
                "report_key": "playtester_step102156_3e2bcddccc66ef00",
                "category_id": "contracts",
                "category_name": "ui_state_desync",
                "title": "Create Contract (3/4) edit_price ('I will receive') stays at 0.00 after Est. Price accept, upButton, and type_text",
                "description": "edit_price stayed at 0.00",
            },
        }
        b_1592 = {
            "report_source": "f12_manual",
            "fingerprint": "d6ad69d0596ad673",
            "stack_hash": -3001000000000000000,
            "stack_key": "playtester:contracts:ui_state_desync",
            "trace": "",
            "kind": "ui_state_desync",
            "subsystem": "contracts",
            "manual_report": {
                "report_key": "playtester_step102161_d6ad69d0596ad673",
                "category_id": "contracts",
                "category_name": "ui_state_desync",
                "title": "CreateContractWnd (3/4) edit_price ('I will receive') remains stuck at '0.00' after ConEstPrice, upButton, and type_text",
                "description": "edit_price stayed at 0.00",
            },
        }
        b_distinct = {
            "report_source": "f12_manual",
            "fingerprint": "1111222233334444",
            "stack_hash": 1111222233334444,
            "stack_key": "playtester:contracts:ui_state_desync",
            "trace": "",
            "kind": "ui_state_desync",
            "subsystem": "contracts",
            "manual_report": {
                "report_key": "playtester_step102170_1111222233334444",
                "category_id": "contracts",
                "category_name": "ui_state_desync",
                "title": "CreateContractWnd (2/4) checkbox select_all fails to toggle item selection",
                "description": "select_all failed",
            },
        }

        class SearchLaggyGitHubCLI(FakeGitHubCLI):
            def __call__(self, cmd, **kwargs):
                # Force `gh issue list --search` to miss created issues (search-index lag simulation)
                if cmd[:3] == ["gh", "issue", "list"]:
                    return subprocess.CompletedProcess(cmd, 0, stdout="[]", stderr="")
                return super().__call__(cmd, **kwargs)

        fake_gh = SearchLaggyGitHubCLI()
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            udir = root / "uploaded"
            qdir.mkdir(parents=True)

            # Sync #1591 first
            (qdir / "01_1591.json").write_text(json.dumps(b_1591), encoding="utf-8")
            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=fake_gh
            ):
                rc1 = sync_mod.main(["--queue-dir", str(qdir), "--uploaded-dir", str(udir)])
            self.assertEqual(rc1, 0)
            self.assertEqual(len(fake_gh.created_issues), 1)
            issue_num = fake_gh.created_issues[0]["number"]

            # Sync #1592 + distinct contracts bug 44s later -> #1592 comments on #1591, distinct opens #2
            (qdir / "02_1592.json").write_text(json.dumps(b_1592), encoding="utf-8")
            (qdir / "03_distinct.json").write_text(json.dumps(b_distinct), encoding="utf-8")
            with mock.patch.object(sync_mod, "check_gh_installed", return_value=True), mock.patch.object(
                sync_mod.subprocess, "run", side_effect=fake_gh
            ):
                rc2 = sync_mod.main(["--queue-dir", str(qdir), "--uploaded-dir", str(udir)])
            self.assertEqual(rc2, 0)
            self.assertEqual(len(fake_gh.created_issues), 2)
            self.assertEqual(len(fake_gh.comments), 1)
            self.assertEqual(fake_gh.comments[0]["number"], issue_num)


if __name__ == "__main__":
    unittest.main()
