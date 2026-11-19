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
    extra: dict | None = None,
) -> dict:
    bundle = {
        "schema_version": 1,
        "report_source": "exception",
        "timestamp": "2026-11-18T12:00:00Z",
        "stack_hash": stack_hash,
        "stack_key": stack_key,
        "trace": trace,
        "session": {
            "character_id": 2100000001,
            "character_name": "Playtester Alpha",
            "solar_system_id": 30000142,
            "solar_system_name": "Jita",
            "ship_type_name": "Rifter",
            "station_id": 60003760,
        },
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
        )
        b2 = _make_crash_bundle(
            stack_hash=-222,
            trace='Traceback (most recent call last):\n  File "b.py", line 2, in g\nValueError: "y"',
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


if __name__ == "__main__":
    unittest.main()
