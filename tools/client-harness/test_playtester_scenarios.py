#!/usr/bin/env python3
"""
Deterministic unit tests for zero-LLM scripted UI `--scenario` mode (GH #1202).
Covers:
  - Static scenario schema & GM/time-control rejection outside declared setup steps
  - Loading & static validation of launch_login, station_ui, and undock_navigate scenarios
  - Every new check type (rpc_sent, notification_received, no_client_exception, no_error_reply,
    state_checks eq/inc/dec, charsel_listed_all_characters, window_populated, ship_mode,
    target_distance, warp_completed, bookmark client+server checks, celestials_in_overview)
  - Fake-hook ScenarioRunner deterministic passes (twice in a row with identical RPC name sets),
    fail-fast abort with state/RPCs/screenshot/failed_check, zero-LLM invocation assertion,
    destiny desync check, and fail-mutation checks for station_ui (wallet RPC error) and
    undock_navigate (stargate jump disabled).
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HARNESS_DIR = Path(__file__).resolve().parent
if str(HARNESS_DIR) not in sys.path:
    sys.path.insert(0, str(HARNESS_DIR))

import playtester  # noqa: E402


EXPECTED_16_STATION_WINDOWS = (
    "shiphangar",
    "itemhangar",
    "fitting",
    "market",
    "industry",
    "agency",
    "charactersheet",
    "skills",
    "wallet",
    "journal",
    "corp",
    "peopleandplaces",
    "mail",
    "notifications",
    "calendar",
    "nes",
)


def _fake_scenario_args(tmp_root: Path, scenario_paths: list[Path], out_dir: Path) -> argparse.Namespace:
    args = playtester.parse_args([
        "--worktree", str(tmp_root),
        "--no-github-sync",
        "--out", str(out_dir),
        *[arg for p in scenario_paths for arg in ("--scenario", str(p))],
    ])
    args.staged_app = str(tmp_root / "staged_app")
    return args


class TestScenarioValidation(unittest.TestCase):
    def test_all_three_stored_scenarios_load_and_pass_static_validation(self):
        scenarios = {}
        for name in ("launch_login", "station_ui", "undock_navigate"):
            path = HARNESS_DIR / "scenarios" / f"{name}.json"
            self.assertTrue(path.exists(), f"Missing scenario file {path}")
            loaded = playtester.load_scenario_file(path)
            self.assertEqual(loaded["id"], name)
            self.assertGreater(len(loaded["steps"]), 0)
            scenarios[name] = loaded

        # launch_login declares extra_characters setup in starting_state only; zero GM slash steps
        self.assertEqual(len(scenarios["launch_login"]["declared_setup_steps"]), 1)
        self.assertEqual(
            scenarios["launch_login"]["declared_setup_steps"][0]["step_id"],
            "starting_state.extra_characters",
        )
        verify_step = next(s for s in scenarios["launch_login"]["steps"] if s["id"] == "verify_hangar_and_ship")
        sc_fields = {
            item["field"]: item
            for item in (verify_step["checks"].get("state_checks") or [])
            if isinstance(item, dict) and item.get("field")
        }
        self.assertIn("wallet_isk", sc_fields)
        self.assertGreaterEqual(sc_fields["wallet_isk"].get("min", 0), 1)
        self.assertIn("trained_skill_count", sc_fields)
        self.assertGreaterEqual(sc_fields["trained_skill_count"].get("min", 0), 1)
        # station_ui and undock_navigate have zero GM/time-control steps
        self.assertEqual(scenarios["station_ui"]["declared_setup_steps"], [])
        self.assertEqual(scenarios["undock_navigate"]["declared_setup_steps"], [])

        opened_targets = [
            s["action"]["target"]
            for s in scenarios["station_ui"]["steps"]
            if s["action"].get("type") == "open_window"
        ]
        self.assertIn("inventory", opened_targets)
        self.assertEqual([t for t in opened_targets if t != "inventory"], list(EXPECTED_16_STATION_WINDOWS))

    def test_rejects_gm_and_time_control_commands_outside_declared_setup_steps(self):
        bad_cases = [
            {"type": "slash_command", "command": "/giveisk 5000000"},
            {"type": "run_slash", "target": "/tr 30000142"},
            {"type": "train_skill", "target": "3300"},
            {"type": "type_text", "target": "chat_input", "command": "/giveitem 587 1"},
            {"type": "type_text", "target": "chat_input", "command": "/teleport 30000142"},
            {"type": "type_text", "target": "chat_input", "command": "/kick pilot"},
            {"type": "type_text", "target": "chat_input", "command": "/suspend pilot"},
            {"type": "type_text", "target": "chat_input", "command": "/downtime 60"},
            {"type": "type_text", "target": "chat_input", "command": "/reinforce 1001"},
            {"type": "station_command", "target": "buy_free_plex_pack"},
            {"type": "advance_server_time", "target": "3600"},
            {"type": "complete_skills", "target": "all"},
        ]
        for bad_act in bad_cases:
            spec = {
                "id": "bad_gm",
                "steps": [{"id": "cheat", "action": bad_act}],
            }
            with self.assertRaises(playtester.ScenarioValidationError, msg=f"Should reject {bad_act!r}"):
                playtester.validate_scenario(spec)

    def test_rejects_unknown_check_keys_and_state_check_operators(self):
        bad_specs = [
            {
                "id": "bad_check_key",
                "steps": [{"id": "s1", "action": {"type": "wait", "wait_ms": 10}, "checks": {"expect_windiw_open": "wallet"}}],
            },
            {
                "id": "bad_wait_until_key",
                "steps": [{"id": "s1", "action": {"type": "wait", "wait_ms": 10}, "wait_until": {"timeout": 1000, "checks": {}}}],
            },
            {
                "id": "bad_wait_until_check_key",
                "steps": [{"id": "s1", "action": {"type": "wait", "wait_ms": 10}, "wait_until": {"checks": {"bogus_key": True}}}],
            },
            {
                "id": "bad_rule_op",
                "steps": [{"id": "s1", "action": {"type": "wait", "wait_ms": 10}, "checks": {"state_checks": [{"field": "wallet_isk", "greater_than": 0}]}}],
            },
        ]
        for bad_spec in bad_specs:
            with self.assertRaises(playtester.ScenarioValidationError, msg=f"Should reject {bad_spec['id']}"):
                playtester.validate_scenario(bad_spec)

        # Explicitly declared setup / time_control steps ARE accepted and listed in declared_setup_steps
        allowed_spec = {
            "id": "declared_setup_ok",
            "steps": [
                {"id": "grant_isk", "setup": True, "action": {"type": "slash_command", "command": "/giveisk 1000"}},
                {"id": "skip_time", "time_control": True, "action": {"type": "advance_server_time", "target": "120"}},
                {"id": "open_wallet", "action": {"type": "open_window", "target": "wallet"}},
            ],
        }
        validated = playtester.validate_scenario(allowed_spec)
        self.assertEqual(
            [s["step_id"] for s in validated["declared_setup_steps"]],
            ["grant_isk", "skip_time"],
        )


class TestScenarioChecks(unittest.TestCase):
    def test_rpc_sent_notification_and_error_exception_checks(self):
        pre = {"session": {"charid": 90000001, "stationid": 60003760}}
        post = {"session": {"charid": 90000001, "stationid": 60003760}}
        fr_ok = {
            "action_ok": True,
            "flight_recorder": {
                "macho_calls": [
                    {"service": "walletMgr", "method": "GetWalletBalance", "status": "OK"},
                    {"service": "marketProxy", "method": "GetOrders", "status": "OK"},
                    {"service": "marketProxy", "method": "GetOrders", "status": "OK"},
                ],
                "scatters_observed": ["OnAccountChange", "OnOrderChanged"],
                "log_exceptions": [],
                "quasar_errors": [],
                "macho_exceptions": [],
            },
        }
        ok, fail_k, msg = playtester.evaluate_scenario_checks(
            {
                "rpc_sent": [
                    {"name": "walletMgr.GetWalletBalance", "min_count": 1, "require_ok": True},
                    {"name": "marketProxy.GetOrders", "min_count": 2},
                ],
                "notification_received": ["OnAccountChange", "OnOrderChanged"],
                "no_client_exception": True,
                "no_error_reply": True,
            },
            pre,
            post,
            fr_ok,
        )
        self.assertTrue(ok, (fail_k, msg))

        ok2, fail_k2, _ = playtester.evaluate_scenario_checks(
            {"rpc_sent": ["contractProxy.GetContracts"]},
            pre,
            post,
            fr_ok,
        )
        self.assertFalse(ok2)
        self.assertEqual(fail_k2, "rpc_sent")

        ok3, fail_k3, _ = playtester.evaluate_scenario_checks(
            {"notification_received": "OnMailReceived"},
            pre,
            post,
            fr_ok,
        )
        self.assertFalse(ok3)
        self.assertEqual(fail_k3, "notification_received")

        fr_qerr = {"action_ok": True, "flight_recorder": {"macho_calls": [], "quasar_errors": [{"error": "timeout"}]}}
        ok4, fail_k4, _ = playtester.evaluate_scenario_checks({"no_error_reply": True}, pre, post, fr_qerr)
        self.assertFalse(ok4)
        self.assertEqual(fail_k4, "no_error_reply")

        fr_exc = {
            "action_ok": True,
            "flight_recorder": {
                "macho_calls": [],
                "log_exceptions": [{"headline": "KeyError: 'foo'", "trace": "Traceback (most recent call last):\nKeyError: 'foo'"}],
            },
        }
        ok5, fail_k5, _ = playtester.evaluate_scenario_checks({"no_client_exception": True}, pre, post, fr_exc)
        self.assertFalse(ok5)
        self.assertEqual(fail_k5, "no_client_exception")

        # Benign V24 fitting validation drag rejection does not fail no_client_exception
        fr_benign_fit = {
            "action_ok": True,
            "flight_recorder": {
                "macho_calls": [],
                "log_exceptions": [{
                    "headline": "Unhandled exception in <TaskletExt ctxt='fittingSlotController.OnDropData'>",
                    "trace": (
                        '  File "eve/client/script/ui/shared/fitting/fittingSlotController.py", line 10, in OnDropData\n'
                        "UserError: ('CustomInfo', {'info': 'FittingValidationFailed: No slots'})\n"
                    ),
                }],
            },
        }
        ok6, fail_k6, msg6 = playtester.evaluate_scenario_checks({"no_client_exception": True}, pre, post, fr_benign_fit)
        self.assertTrue(ok6, (fail_k6, msg6))

    def test_state_value_window_populated_charsel_and_space_navigation_checks(self):
        pre = {
            "session": {"solarsystemid2": 30000142, "stationid": None},
            "wallet_state": {"isk_balance": 100000.0},
            "inventory_state": {"hangar_item_count": 3},
            "skills_state": {"queue_len": 1, "total_trained_skills": 12},
            "market_state": {"char_orders_count": 2},
            "warp_telemetry": {"started_count": 0, "finished_count": 0, "in_warp": False},
            "space": {
                "ego": {"mode": 2, "speed_mps": 0.0},
                "nearby_balls": [{"item_id": 60003760, "distance_m": 4500.0}],
            },
        }
        post = {
            "session": {"solarsystemid2": 30000142, "stationid": None},
            "active_view": "inflight",
            "wallet_state": {"isk_balance": 125000.0},
            "inventory_state": {"hangar_item_count": 2},
            "skills_state": {"queue_len": 2, "total_trained_skills": 12},
            "market_state": {"char_orders_count": 3},
            "charsel_state": {"active": True, "count": 2, "names": ["Aura Capsuleer", "Aura Two"]},
            "bookmarks_state": {"count": 1, "names": ["Scenario Planet Spot"]},
            "warp_telemetry": {"started_count": 1, "finished_count": 1, "in_warp": False, "saw_signal": True},
            "open_windows": [
                {
                    "window_id": "wallet",
                    "name": "wallet",
                    "cls": "WalletWindow",
                    "caption": "Wallet",
                    "has_loading": False,
                    "has_error": False,
                    "tab_count": 3,
                    "row_count": 2,
                    "populated_count": 5,
                    "sample_texts": ["125,000.00 ISK"],
                }
            ],
            "space": {
                "ego": {"mode": 1, "speed_mps": 215.0},
                "celestials_in_overview": True,
                "nearby_balls": [
                    {"item_id": 60003760, "distance_m": 1800.0},
                    {"item_id": 40009077, "distance_m": 25000.0},
                ],
            },
        }
        exec_ok = {
            "action_ok": True,
            "warp_telemetry": {"saw_signal": True, "started_count": 1, "finished_count": 1, "in_warp": False},
            "flight_recorder": {
                "macho_calls": [{"service": "walletMgr", "method": "GetWalletBalance", "status": "OK"}],
                "scatters_observed": [],
                "log_exceptions": [],
                "quasar_errors": [],
                "macho_exceptions": [],
            },
        }
        ok, fk, msg = playtester.evaluate_scenario_checks(
            {
                "charsel_listed_all_characters": ["Aura Capsuleer", "Aura Two"],
                "window_populated": {"window": "wallet", "min_populated": 1, "contains_text": "125,000"},
                "ship_mode": 1,
                "target_distance": {"target_id": 60003760, "decreased": 1000.0},
                "warp_completed": {"target_id": 40009077, "landing_distance_max_m": 150000},
                "bookmark_present": "Scenario Planet Spot",
                "bookmark_server_check": "Scenario Planet Spot",
                "celestials_in_overview": True,
                "state_checks": [
                    {"field": "wallet_isk", "increased": 25000.0, "equals": 125000.0},
                    {"field": "hangar_item_count", "decreased": 1},
                    {"field": "skill_queue_len", "delta": 1},
                    {"field": "order_count", "equals": 3},
                ],
            },
            pre,
            post,
            exec_ok,
            context={"server_bookmark_exists": lambda bm: bm == "Scenario Planet Spot"},
        )
        self.assertTrue(ok, (fk, msg))

        # Empty loading window fails window_populated
        bad_win_post = dict(post, open_windows=[{
            "window_id": "wallet", "has_loading": True, "has_error": False, "populated_count": 0,
        }])
        ok_w, fk_w, _ = playtester.evaluate_scenario_checks(
            {"window_populated": "wallet"}, pre, bad_win_post, {"action_ok": True, "flight_recorder": {}}
        )
        self.assertFalse(ok_w)
        self.assertEqual(fk_w, "window_populated")

        # Missing server bookmark fails bookmark_server_check
        ok_bm, fk_bm, _ = playtester.evaluate_scenario_checks(
            {"bookmark_server_check": "Scenario Planet Spot"},
            pre,
            post,
            exec_ok,
            context={"server_bookmark_exists": lambda _bm: False},
        )
        self.assertFalse(ok_bm)
        self.assertEqual(fk_bm, "bookmark_server_check")


class TestScenarioRunnerFakeHook(unittest.TestCase):
    def _run_with_fake_rpc(self, scenario_files, out_dir, handler, tmp_root, extra_client_log_text=None):
        def _fake_start_server(self_runner, reset_db=False):
            self_runner.current_run_dir.mkdir(parents=True, exist_ok=True)
            (self_runner.current_run_dir / "server.log").write_text(
                "2026-11-16T00:00:00Z INFO authentication Login account=demo session=1\n",
                encoding="utf-8",
            )
            return True

        def _fake_start_client(self_runner):
            clogs_dir = self_runner.current_run_dir / "client_logs"
            clogs_dir.mkdir(parents=True, exist_ok=True)
            log_body = extra_client_log_text or "2026-11-16 EVE Client version 24.01 started\n"
            (clogs_dir / "eve_client.log").write_text(log_body, encoding="utf-8")
            self_runner._last_boot_result = handler("bootstrap_session", {})
            return True

        def _fake_capture(dest_path, **_kw):
            Path(dest_path).parent.mkdir(parents=True, exist_ok=True)
            Path(dest_path).write_bytes(b"\x89PNG\r\n\x1a\nFAKE")
            return {"ok": True, "path": str(dest_path)}

        args = _fake_scenario_args(tmp_root, scenario_files, out_dir)
        with mock.patch.object(playtester.AgyStreamWorkerPool, "__init__", side_effect=AssertionError("LLM pool must never run")), \
             mock.patch.object(playtester, "invoke_agy_planner", side_effect=AssertionError("LLM planner must never run")), \
             mock.patch.object(playtester, "invoke_agy_executor", side_effect=AssertionError("LLM executor must never run")), \
             mock.patch.object(playtester.ScenarioRunner, "start_server", _fake_start_server), \
             mock.patch.object(playtester.ScenarioRunner, "stop_server", return_value=None), \
             mock.patch.object(playtester.ScenarioRunner, "start_client", _fake_start_client), \
             mock.patch.object(playtester.ScenarioRunner, "stop_client", return_value=None), \
             mock.patch.object(playtester.ScenarioRunner, "_provision_extra_characters", return_value=["Aura Capsuleer", "Aura Two"]), \
             mock.patch.object(playtester, "capture_window_screenshot", side_effect=_fake_capture), \
             mock.patch.object(playtester, "get_git_head", return_value="7e3600c29"):
            runner = playtester.ScenarioRunner(args)
            runner.rpc = types_rpc = mock.MagicMock()
            types_rpc.call.side_effect = lambda cmd, params=None, timeout=20.0: handler(cmd, params or {})
            return runner.run_scenarios()

    def test_three_step_login_open_close_passes_twice_deterministically_with_zero_llm(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen_path = root / "mini_inventory.json"
            scen_path.write_text(
                json.dumps({
                    "id": "mini_inventory",
                    "starting_state": {"account": "demo", "char_id": 90000001},
                    "steps": [
                        {
                            "id": "login_settled",
                            "action": {"type": "wait", "wait_ms": 100},
                            "checks": {"rpc_sent": ["charUnboundMgr.SelectCharacterID"], "expect_session_field": "charid==90000001"},
                        },
                        {
                            "id": "open_inventory",
                            "action": {"type": "open_window", "target": "inventory"},
                            "checks": {"expect_window_open": "inventory", "window_populated": "inventory"},
                        },
                        {
                            "id": "close_inventory",
                            "action": {"type": "close_window", "target": "inventory"},
                            "checks": {"expect_window_closed": "inventory"},
                        },
                    ],
                }),
                encoding="utf-8",
            )

            win_open = {"value": False}

            def _handler(cmd, params):
                if cmd == "bootstrap_session":
                    win_open["value"] = False
                    return {
                        "ok": True,
                        "state": {
                            "client_version": "24.01",
                            "client_build": "9990001",
                            "session": {"charid": 90000001, "stationid": 60003760},
                            "active_view": "hangar",
                            "open_windows": [],
                        },
                        "flight_recorder": {
                            "macho_calls": [{"service": "charUnboundMgr", "method": "SelectCharacterID", "status": "OK"}],
                        },
                    }
                act = (params or {}).get("action") or {}
                atype = act.get("type")
                calls = []
                if atype == "open_window":
                    win_open["value"] = True
                    calls = [{"service": "invbroker", "method": "List", "status": "OK"}]
                elif atype == "close_modal_or_window" or atype == "close_window":
                    win_open["value"] = False
                windows = (
                    [{"window_id": "inventory", "has_loading": False, "has_error": False, "populated_count": 3}]
                    if win_open["value"]
                    else []
                )
                return {
                    "action_ok": True,
                    "post_state": {
                        "client_version": "24.01",
                        "client_build": "9990001",
                        "session": {"charid": 90000001, "stationid": 60003760},
                        "active_view": "hangar",
                        "open_windows": windows,
                    },
                    "flight_recorder": {"macho_calls": calls, "scatters_observed": [], "log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                }

            out1 = root / "out1"
            out2 = root / "out2"
            rc1 = self._run_with_fake_rpc([scen_path], out1, _handler, root)
            rc2 = self._run_with_fake_rpc([scen_path], out2, _handler, root)
            self.assertEqual(rc1, 0)
            self.assertEqual(rc2, 0)

            res1 = json.loads((out1 / "result.json").read_text(encoding="utf-8"))
            res2 = json.loads((out2 / "result.json").read_text(encoding="utf-8"))
            self.assertTrue(res1["journeys"]["mini_inventory"]["passed"])
            self.assertTrue(res1["journeys"]["mini_inventory"]["ui_scenario"])
            self.assertEqual(res1["journeys"]["mini_inventory"]["log_errors"], [])
            self.assertEqual(res1["journeys"]["mini_inventory"]["client_build"], 9990001)
            self.assertEqual(res1["client_build"], 9990001)
            self.assertEqual(res1["server_commit"], "7e3600c29")
            self.assertEqual(res1["journeys"]["mini_inventory"]["step_rpcs"], res2["journeys"]["mini_inventory"]["step_rpcs"])

    def test_failing_window_check_stops_scenario_and_captures_state_rpcs_screenshot(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen_path = root / "mini_fail.json"
            scen_path.write_text(
                json.dumps({
                    "id": "mini_fail",
                    "starting_state": {"account": "demo"},
                    "steps": [
                        {"id": "step_open", "action": {"type": "open_window", "target": "inventory"}, "checks": {"expect_window_open": "inventory"}},
                        {"id": "step_close_never_runs", "action": {"type": "close_window", "target": "inventory"}},
                    ],
                }),
                encoding="utf-8",
            )
            executed_steps = []

            def _handler(cmd, params):
                if cmd == "bootstrap_session":
                    return {"ok": True, "state": {"client_build": "9990001", "session": {"charid": 90000001}, "open_windows": []}, "flight_recorder": {"macho_calls": []}}
                if cmd == "execute_action":
                    executed_steps.append((params.get("action") or {}).get("type"))
                return {
                    "action_ok": True,
                    "post_state": {"client_build": "9990001", "session": {"charid": 90000001}, "open_windows": []},
                    "flight_recorder": {"macho_calls": [{"service": "invbroker", "method": "GetInventory", "status": "OK"}], "scatters_observed": [], "log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                }

            out = root / "out_fail"
            rc = self._run_with_fake_rpc([scen_path], out, _handler, root)
            self.assertNotEqual(rc, 0)
            self.assertEqual(len(executed_steps), 1)
            res = json.loads((out / "result.json").read_text(encoding="utf-8"))
            j = res["journeys"]["mini_fail"]
            self.assertFalse(j["passed"])
            self.assertEqual(j["failure"]["step_id"], "step_open")
            self.assertEqual(j["failure"]["failed_check"], "expect_window_open")
            self.assertIn("invbroker.GetInventory", j["failure"]["last_rpcs"])
            self.assertTrue(Path(j["failure"]["screenshot"]).exists())

    def test_station_ui_mutation_wallet_rpc_error_fails_at_wallet_step(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen_path = HARNESS_DIR / "scenarios" / "station_ui.json"
            open_w = {"target": None}
            alias_to_win = {"peopleandplaces": "people", "nes": "store"}

            def _handler(cmd, params):
                if cmd == "bootstrap_session":
                    return {"ok": True, "state": {"client_build": "9990001", "session": {"charid": 90000001, "stationid": 60003760}, "active_view": "hangar", "open_windows": []}, "flight_recorder": {"macho_calls": []}}
                act = (params or {}).get("action") or {}
                atype = act.get("type")
                tgt = str(act.get("target") or "")
                calls = []
                macho_excs = []
                if atype == "open_window":
                    wname = alias_to_win.get(tgt, tgt)
                    open_w["target"] = wname
                    if tgt == "wallet":
                        calls = [{"service": "walletMgr", "method": "GetWalletBalance", "status": "ERR", "error": "SimulatedWalletFailure"}]
                        macho_excs = [{"service": "walletMgr", "method": "GetWalletBalance", "error": "SimulatedWalletFailure"}]
                    else:
                        calls = [{"service": f"{wname}Svc", "method": "Fetch", "status": "OK"}]
                elif atype == "close_window":
                    open_w["target"] = None
                wins = (
                    [{"window_id": open_w["target"], "has_loading": False, "has_error": False, "populated_count": 2}]
                    if open_w["target"]
                    else []
                )
                return {
                    "action_ok": True,
                    "post_state": {"client_build": "9990001", "session": {"charid": 90000001, "stationid": 60003760}, "active_view": "hangar", "open_windows": wins},
                    "flight_recorder": {"macho_calls": calls, "scatters_observed": [], "log_exceptions": [], "quasar_errors": [], "macho_exceptions": macho_excs},
                }

            out = root / "out_mut_wallet"
            rc = self._run_with_fake_rpc([scen_path], out, _handler, root)
            self.assertNotEqual(rc, 0)
            res = json.loads((out / "result.json").read_text(encoding="utf-8"))
            self.assertEqual(res["journeys"]["station_ui"]["failure"]["step_id"], "open_wallet")
            self.assertEqual(res["journeys"]["station_ui"]["failure"]["failed_check"], "no_error_reply")

    def test_undock_navigate_mutation_disabled_stargate_fails_at_first_gate(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen_path = HARNESS_DIR / "scenarios" / "undock_navigate.json"
            state = {
                "client_build": "9990001",
                "session": {"charid": 90000001, "stationid": 60003760, "solarsystemid2": 30000142},
                "active_view": "hangar",
                "bookmarks_state": {"count": 0, "names": []},
                "autopilot_state": {"active": False, "waypoints": [], "route": []},
                "warp_telemetry": {"started_count": 0, "finished_count": 0, "in_warp": False},
                "space": {
                    "ego": {"mode": 2, "speed_mps": 0.0},
                    "selected_item": {"item_id": 60003760},
                    "celestials_in_overview": True,
                    "nearby_balls": [
                        {"item_id": 60003760, "distance_m": 5000.0},
                        {"item_id": 40009077, "distance_m": 1e11},
                        {"item_id": 40009078, "distance_m": 2e11},
                    ],
                },
            }

            def _snap():
                return json.loads(json.dumps(state))

            def _handler(cmd, params):
                if cmd == "bootstrap_session":
                    return {"ok": True, "state": _snap(), "flight_recorder": {"macho_calls": []}}
                act = (params or {}).get("action") or {}
                atype = act.get("type")
                if atype == "undock":
                    state["session"]["stationid"] = None
                    state["active_view"] = "inflight"
                elif atype == "select_overview":
                    state["space"]["selected_item"] = {"item_id": 60003760}
                elif atype == "approach":
                    state["space"]["ego"] = {"mode": 1, "speed_mps": 210.0}
                    state["space"]["nearby_balls"][0]["distance_m"] = 2000.0
                elif atype == "orbit":
                    state["space"]["ego"] = {"mode": 4, "speed_mps": 240.0}
                    state["space"]["nearby_balls"][0]["distance_m"] = 4800.0
                elif atype == "keep_at_range":
                    state["space"]["ego"] = {"mode": 1, "speed_mps": 220.0}
                    state["space"]["nearby_balls"][0]["distance_m"] = 9000.0
                elif atype == "stop":
                    state["space"]["ego"] = {"mode": 2, "speed_mps": 0.0}
                elif atype == "align_to":
                    state["space"]["ego"] = {"mode": 1, "speed_mps": 95.0}
                elif atype in ("warp_to", "warp_to_location"):
                    state["warp_telemetry"]["started_count"] += 1
                    state["warp_telemetry"]["finished_count"] += 1
                    state["warp_telemetry"]["in_warp"] = False
                    state["warp_telemetry"]["saw_signal"] = True
                    tgt = act.get("item_id")
                    for b in state["space"]["nearby_balls"]:
                        if b["item_id"] == tgt:
                            b["distance_m"] = 5000.0
                elif atype == "save_location":
                    bm = str(act.get("target") or "Scenario Planet Spot")
                    state["bookmarks_state"] = {"count": 1, "names": [bm]}
                elif atype == "set_destination":
                    state["autopilot_state"]["waypoints"] = [30001407]
                    state["autopilot_state"]["route"] = [30000142, 30001363, 30001405, 30001407]
                elif atype == "engage_autopilot":
                    # Stargates disabled: autopilot turns on, but session.solarsystemid2 never leaves 30000142!
                    state["autopilot_state"]["active"] = True
                return {
                    "action_ok": True,
                    "post_state": _snap(),
                    "warp_telemetry": dict(state["warp_telemetry"]),
                    "flight_recorder": {"macho_calls": [{"service": "beyonce", "method": str(atype), "status": "OK"}], "scatters_observed": [], "log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                }

            # Mutate first gate wait timeout down to 400ms so test finishes instantly
            scen_mut = root / "undock_mut_gate.json"
            raw_scen = json.loads(scen_path.read_text(encoding="utf-8"))
            for st in raw_scen["steps"]:
                if st["id"] == "wait_gate_1_sobaseki":
                    st["wait_until"]["timeout_ms"] = 400
                    st["wait_until"]["poll_interval_ms"] = 250
            scen_mut.write_text(json.dumps(raw_scen), encoding="utf-8")

            out = root / "out_mut_gate"
            with mock.patch.object(playtester.ScenarioRunner, "_server_bookmark_exists", return_value=True):
                rc = self._run_with_fake_rpc([scen_mut], out, _handler, root)
            self.assertNotEqual(rc, 0)
            res = json.loads((out / "result.json").read_text(encoding="utf-8"))
            self.assertEqual(res["journeys"]["undock_navigate"]["failure"]["step_id"], "wait_gate_1_sobaseki")

    def test_destiny_desync_log_causes_scenario_failure(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen_path = root / "mini_desync.json"
            scen_path.write_text(
                json.dumps({
                    "id": "mini_desync",
                    "steps": [{"id": "ok_step", "action": {"type": "wait", "wait_ms": 50}}],
                }),
                encoding="utf-8",
            )

            def _handler(_cmd, _params):
                return {
                    "ok": True,
                    "action_ok": True,
                    "state": {"client_build": "9990001", "session": {"charid": 90000001}},
                    "post_state": {"client_build": "9990001", "session": {"charid": 90000001}},
                    "flight_recorder": {"macho_calls": []},
                }

            out = root / "out_desync"
            with mock.patch.object(
                playtester.ScenarioRunner,
                "_run_destiny_parity_check",
                return_value={"stale_at_action": 2, "stale_at_history": 0},
            ):
                rc = self._run_with_fake_rpc([scen_path], out, _handler, root)
            self.assertNotEqual(rc, 0)
            res = json.loads((out / "result.json").read_text(encoding="utf-8"))
            self.assertFalse(res["journeys"]["mini_desync"]["passed"])
            self.assertEqual(res["journeys"]["mini_desync"]["failure"]["failed_check"], "clean_logs")


class TestScenarioReviewFixes(unittest.TestCase):
    def _run_with_step_hook(
        self,
        tmp_root: Path,
        scenario_files: list,
        out_dir: Path,
        step_handler,
        init_client_logs=None,
    ) -> int:
        def _fake_start_server(self_runner, reset_db: bool = False):
            _ = reset_db
            self_runner.current_run_dir.mkdir(parents=True, exist_ok=True)
            (self_runner.current_run_dir / "server.log").write_text(
                "2026-11-16T00:00:00Z INFO Listening on Native machoNet / GPSTransport TCP\n",
                encoding="utf-8",
            )
            return True

        def _fake_start_client(self_runner):
            clogs_dir = self_runner.current_run_dir / "client_logs"
            clogs_dir.mkdir(parents=True, exist_ok=True)
            (clogs_dir / "eve_client.log").write_text(
                "2026-11-16 EVE Client version 24.01 started\n",
                encoding="utf-8",
            )
            if callable(init_client_logs):
                init_client_logs(self_runner.current_run_dir, clogs_dir)
            self_runner._last_boot_result = {
                "ok": True,
                "state": {
                    "client_version": "24.01",
                    "client_build": "9990001",
                    "session": {"charid": 90000001, "stationid": 60003760},
                    "active_view": "hangar",
                    "open_windows": [],
                },
                "flight_recorder": {"macho_calls": []},
            }
            return True

        def _fake_capture(dest_path, **_kw):
            Path(dest_path).parent.mkdir(parents=True, exist_ok=True)
            Path(dest_path).write_bytes(b"\x89PNG\r\n\x1a\nFAKE")
            return {"ok": True, "path": str(dest_path)}

        args = _fake_scenario_args(tmp_root, scenario_files, out_dir)
        with mock.patch.object(playtester.AgyStreamWorkerPool, "__init__", side_effect=AssertionError("LLM pool must never run")), \
             mock.patch.object(playtester, "invoke_agy_planner", side_effect=AssertionError("LLM planner must never run")), \
             mock.patch.object(playtester, "invoke_agy_executor", side_effect=AssertionError("LLM executor must never run")), \
             mock.patch.object(playtester.ScenarioRunner, "start_server", _fake_start_server), \
             mock.patch.object(playtester.ScenarioRunner, "stop_server", return_value=None), \
             mock.patch.object(playtester.ScenarioRunner, "start_client", _fake_start_client), \
             mock.patch.object(playtester.ScenarioRunner, "stop_client", return_value=None), \
             mock.patch.object(playtester.ScenarioRunner, "_provision_extra_characters", return_value=[]), \
             mock.patch.object(playtester, "capture_window_screenshot", side_effect=_fake_capture), \
             mock.patch.object(playtester, "get_git_head", return_value="7e3600c29"):
            runner = playtester.ScenarioRunner(args)
            runner.rpc = mock.MagicMock()
            runner.rpc.call.side_effect = lambda cmd, params=None, timeout=20.0: step_handler(runner, cmd, params or {})
            return runner.run_scenarios()

    def test_stored_scenarios_cover_inventory_window_closed_assertions_and_stop_step(self):
        station = json.loads((HARNESS_DIR / "scenarios" / "station_ui.json").read_text(encoding="utf-8"))
        close_steps = [s for s in station["steps"] if s["action"].get("type") == "close_window"]
        self.assertGreaterEqual(len(close_steps), 17)
        for cs in close_steps:
            self.assertTrue(
                cs.get("checks", {}).get("expect_window_closed"),
                f"missing expect_window_closed on {cs['id']}",
            )
        notif_open = next(s for s in station["steps"] if s["id"] == "open_notifications")
        self.assertEqual(notif_open["checks"].get("expect_window_open"), "notifications")
        self.assertEqual(notif_open["checks"]["window_populated"].get("min_populated"), 0)
        self.assertFalse(notif_open["checks"]["window_populated"].get("require_rpc_or_data", True))
        notif_close = next(s for s in station["steps"] if s["id"] == "close_notifications")
        self.assertEqual(notif_close["action"].get("target"), "notifications")
        self.assertEqual(notif_close["checks"].get("expect_window_closed"), "notifications")

        undock = json.loads((HARNESS_DIR / "scenarios" / "undock_navigate.json").read_text(encoding="utf-8"))
        stop_steps = [s for s in undock["steps"] if s["action"].get("type") == "stop"]
        self.assertEqual(len(stop_steps), 1)
        self.assertEqual(stop_steps[0]["checks"].get("ship_mode"), 2)

        login = json.loads((HARNESS_DIR / "scenarios" / "launch_login.json").read_text(encoding="utf-8"))
        self.assertEqual([s["id"] for s in login["steps"]], ["wait_for_charsel", "select_test_character", "verify_hangar_and_ship"])

    def test_rejects_extended_gm_slash_prefixes_in_type_text(self):
        for slash_cmd in (
            "/moveme 30000142",
            "/translocate 60003760",
            "/livingworld status",
            "/unspawn 9001",
            "/despawn 9001",
            "/ban demo",
            "/role gm",
        ):
            spec = {
                "id": "gm_text_reject",
                "steps": [{"id": "s1", "action": {"type": "type_text", "target": "chat", "command": slash_cmd}}],
            }
            with self.assertRaises(playtester.ScenarioValidationError, msg=slash_cmd):
                playtester.validate_scenario(spec)

    def test_oracle_noammoloaded_minimap_and_fitting_validation_filters(self):
        self.assertTrue(
            playtester.PlaytesterBugOracle.is_benign_client_traceback(
                "AttributeError: 'MapSlider' object has no attribute 'minimumWidth'",
                '  File "eve/client/script/ui/shared/maps/minimap.py", line 123, in _ResizeSlider\n'
                "AttributeError: 'MapSlider' object has no attribute 'minimumWidth'\n",
            )
        )
        ammo_hl = "Unhandled exception in <TaskletExt ctxt='ModuleButton.OnStateChange'>"
        ammo_tb = (
            '  File "eve/client/script/ui/inflight/shipModuleButton/shipModuleButton.py", line 420, in OnStateChange\n'
            '  File "eve/client/script/ui/inflight/shipModuleButton/shipModuleButton.py", line 435, in activateeffect\n'
            "UserError: ('NoAmmoLoaded', {})\n"
        )
        self.assertFalse(playtester.PlaytesterBugOracle._is_unhandled_tasklet_traceback(ammo_hl, ammo_tb))
        self.assertTrue(playtester.PlaytesterBugOracle.is_ignorable_client_traceback(ammo_hl, ammo_tb))

        fit_hl = "Unhandled exception in <TaskletExt ctxt='fittingSlotController.OnDropData'>"
        fit_tb = (
            '  File "eve/client/script/ui/shared/fitting/fittingSlotController.py", line 188, in OnDropData\n'
            "UserError: ('CustomInfo', {'info': 'FittingValidationFailed: Invalid slot'})\n"
        )
        self.assertFalse(playtester.PlaytesterBugOracle._is_unhandled_tasklet_traceback(fit_hl, fit_tb))
        self.assertTrue(playtester.PlaytesterBugOracle.is_ignorable_client_traceback(fit_hl, fit_tb))

        generic_hl = "Unhandled exception in <TaskletExt ctxt='genericSvc.DoThing'>"
        generic_tb = (
            '  File "eve/client/script/ui/services/genericSvc.py", line 40, in DoThing\n'
            "UserError: ('CustomInfo', {'info': 'UnexpectedServerFailure'})\n"
        )
        self.assertTrue(playtester.PlaytesterBugOracle._is_unhandled_tasklet_traceback(generic_hl, generic_tb))
        self.assertFalse(playtester.PlaytesterBugOracle.is_ignorable_client_traceback(generic_hl, generic_tb))

    def test_state_field_aliases_and_subset_prefix_rules(self):
        st = {
            "skills_state": {"total_trained_skills": 42},
            "inventory_state": {"ship_hangar_count": 3},
            "space": {"ego": {"signal_in_warp": True}},
            "bookmarks_state": {"names": ["Home", "Gate Spot"]},
            "autopilot_state": {"route": [30000142, 30001363, 30001407]},
            "active_view": "inflight_space",
        }
        self.assertEqual(playtester._extract_state_value(st, "trained_skill_count"), 42)
        self.assertEqual(playtester._extract_state_value(st, "ship_hangar_count"), 3)
        self.assertIs(playtester._extract_state_value(st, "ship_warping"), True)

        ok, fk, msg = playtester.evaluate_scenario_checks(
            {
                "state_checks": [
                    {"field": "trained_skill_count", "equals": 42},
                    {"field": "ship_hangar_count", "equals": 3},
                    {"field": "ship_warping", "equals": True},
                    {"field": "bookmarks_state.names", "subset": ["Home", "Gate Spot", "Extra"]},
                    {"field": "autopilot_route", "prefix": [30000142, 30001363]},
                    {"field": "active_view", "prefix": "inflight"},
                ]
            },
            st,
            st,
            {"action_ok": True, "flight_recorder": {}},
        )
        self.assertTrue(ok, (fk, msg))

        bad_sub, fk_sub, _ = playtester.evaluate_scenario_checks(
            {"state_checks": [{"field": "bookmarks_state.names", "subset": ["Home"]}]},
            st,
            st,
            {"action_ok": True, "flight_recorder": {}},
        )
        self.assertFalse(bad_sub)
        self.assertEqual(fk_sub, "state_checks.bookmarks_state.names")

        bad_pfx, fk_pfx, _ = playtester.evaluate_scenario_checks(
            {"state_checks": [{"field": "autopilot_route", "prefix": [30000142, 30009999]}]},
            st,
            st,
            {"action_ok": True, "flight_recorder": {}},
        )
        self.assertFalse(bad_pfx)
        self.assertEqual(fk_pfx, "state_checks.autopilot_route")

    def test_server_handshake_seen_and_select_character_step_timeout(self):
        self.assertEqual(playtester.ScenarioRunner._step_rpc_timeout({"type": "select_character"}), 35.0)
        self.assertEqual(playtester.ScenarioRunner._step_rpc_timeout({"type": "warp_to_location"}), 35.0)
        self.assertEqual(playtester.ScenarioRunner._step_rpc_timeout({"type": "warp_to_bookmark"}), 35.0)
        self.assertEqual(playtester.ScenarioRunner._step_rpc_timeout({"type": "open_window"}), 20.0)

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen = root / "s.json"
            scen.write_text(json.dumps({"id": "s", "steps": [{"id": "w", "action": {"type": "wait", "wait_ms": 10}}]}), encoding="utf-8")
            args = _fake_scenario_args(root, [scen], root / "out")
            runner = playtester.ScenarioRunner(args)
            runner.current_run_dir = root / "run1"
            runner.current_run_dir.mkdir(parents=True, exist_ok=True)
            slog = runner.current_run_dir / "server.log"

            pre_launch = (
                "2026-11-16T00:00:00Z INFO Listening on Native machoNet / GPSTransport TCP\n"
                "2026-11-16T00:00:01Z INFO Accepted Client Bridge connection\n"
                "2026-11-16T00:00:01Z INFO Dispatching RPC call user_id=Some(1) service=charUnboundMgr method=GetCharactersToSelect\n"
            )
            slog.write_text(pre_launch, encoding="utf-8")
            runner._client_launch_server_log_offset = slog.stat().st_size
            self.assertFalse(runner._server_handshake_seen("demo"))

            with slog.open("a", encoding="utf-8") as f:
                f.write("2026-11-16T00:00:02Z INFO Accepted native machoNet connection peer=127.0.0.1:51234\n")
            self.assertFalse(runner._server_handshake_seen("demo"))

            with slog.open("a", encoding="utf-8") as f:
                f.write("2026-11-16T00:00:03Z INFO Dispatching RPC call user_id=Some(1) service=charUnboundMgr method=GetCharacterSelectionData\n")
            self.assertTrue(runner._server_handshake_seen("demo"))

    def test_populated_client_tracebacks_log_unhandled_fails_and_benign_ignored(self):
        benign_tb = (
            "EXCEPTION #1 logged at 11/16/2026 00:00:01 : unhandled exception\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/maps/minimap.py", line 123, in _ResizeSlider\n'
            "AttributeError: 'MapSlider' object has no attribute 'minimumWidth'\n"
            "EXCEPTION END\n"
            "EXCEPTION #2 logged at 11/16/2026 00:00:02 : Unhandled exception in <TaskletExt ctxt='fittingSlotController.OnDropData'>\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/fitting/fittingSlotController.py", line 188, in OnDropData\n'
            "UserError: ('CustomInfo', {'info': 'FittingValidationFailed: No slots'})\n"
            "EXCEPTION END\n"
        )
        unhandled_tb = (
            "EXCEPTION #3 logged at 11/16/2026 00:00:03 : error\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/broken.py", line 10, in Render\n'
            "KeyError: 'broken_ui'\n"
            "EXCEPTION END\n"
        )
        blocks = playtester.PlaytesterBugOracle.parse_tb_blocks(benign_tb + unhandled_tb)
        self.assertEqual(len(blocks), 3)

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen = root / "tb_check.json"
            scen.write_text(
                json.dumps({
                    "id": "tb_check",
                    "steps": [{"id": "step_one", "action": {"type": "wait", "wait_ms": 10}, "checks": {"no_client_exception": True}}],
                }),
                encoding="utf-8",
            )

            def _ok_step(_runner, _cmd, _params):
                return {
                    "action_ok": True,
                    "post_state": {"client_build": "9990001", "session": {"charid": 90000001}},
                    "flight_recorder": {"macho_calls": [], "scatters_observed": [], "log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                }

            out_benign = root / "out_benign"
            rc_b = self._run_with_step_hook(
                root,
                [scen],
                out_benign,
                _ok_step,
                init_client_logs=lambda rd, _cl: (rd / "client_tracebacks.log").write_text(benign_tb, encoding="utf-8"),
            )
            self.assertEqual(rc_b, 0)
            res_b = json.loads((out_benign / "result.json").read_text(encoding="utf-8"))
            self.assertTrue(res_b["journeys"]["tb_check"]["passed"])

            out_bad = root / "out_bad"
            rc_u = self._run_with_step_hook(
                root,
                [scen],
                out_bad,
                _ok_step,
                init_client_logs=lambda rd, _cl: (rd / "client_tracebacks.log").write_text(benign_tb + unhandled_tb, encoding="utf-8"),
            )
            self.assertEqual(rc_u, 1)
            res_u = json.loads((out_bad / "result.json").read_text(encoding="utf-8"))
            self.assertFalse(res_u["journeys"]["tb_check"]["passed"])
            self.assertEqual(res_u["journeys"]["tb_check"]["failure"]["step_id"], "step_one")
            self.assertEqual(res_u["journeys"]["tb_check"]["failure"]["failed_check"], "no_client_exception")

    def test_evomu_site_logexception_blocks_in_client_log_and_scen_dir_tracebacks_log(self):
        benign_evomu_blocks = (
            "2026-11-16 00:00:01 EVE Client version 24.02 started\n"
            "[evomu_site] logmodule.LogException args=(AttributeError(\"'MapSlider' object has no attribute 'minimumWidth'\"),) kwargs={}\n"
            "INNER TRACEBACK:\n"
            '  File "packages/map/client/minimap.py", line 219, in _ResizeSlider\n'
            "Traceback (most recent call last):\n"
            "None\n"
            "CALLSTACK:\n"
            '  File "packages/logmodule/__init__.py", line 50, in LogException\n'
            "[evomu_site] logmodule.LogException args=(UserError('CustomInfo', {'info': 'FittingValidationFailed: No slots'}),) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/fitting/fittingSlotController.py", line 188, in OnDropData\n'
            "UserError: ('CustomInfo', {'info': 'FittingValidationFailed: No slots'})\n"
            "CALLSTACK:\n"
            '  File "carbon/common/script/util/bluepy.py", line 12, in TaskletExt\n'
            "[evomu_site] logmodule.LogException args=(UserError('NotEnoughCargoSpace', {}),) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/inventory/invWindow.py", line 812, in _MoveItems\n'
            "UserError: User error, msg=NotEnoughCargoSpace, dict={}\n"
            "CALLSTACK:\n"
            '  File "carbon/common/script/util/bluepy.py", line 15, in TaskletExt\n'
            "INFO client UI steady state tick\n"
        )
        unhandled_evomu_client_log = (
            "[evomu_site] logmodule.LogException args=(RuntimeError('real_client_bug'),) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/brokenPanel.py", line 77, in LoadData\n'
            "RuntimeError: real_client_bug\n"
            "CALLSTACK:\n"
            '  File "carbon/common/script/util/bluepy.py", line 20, in TaskletExt\n'
        )
        unhandled_evomu_tb_log = (
            "logmodule.LogException args=(RuntimeError('boom_in_scen_dir_tb'),) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/brokenPanel.py", line 88, in Open\n'
            "RuntimeError: boom_in_scen_dir_tb\n"
            "CALLSTACK:\n"
            '  File "carbon/common/script/util/bluepy.py", line 22, in TaskletExt\n'
        )
        self.assertEqual(len(playtester.PlaytesterBugOracle.parse_tb_blocks(benign_evomu_blocks)), 3)

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen = root / "evomu_site_logs.json"
            scen.write_text(
                json.dumps({
                    "id": "evomu_site_logs",
                    "steps": [{"id": "step_one", "action": {"type": "wait", "wait_ms": 10}, "checks": {"no_client_exception": True}}],
                }),
                encoding="utf-8",
            )

            def _ok_step(_runner, _cmd, _params):
                return {
                    "action_ok": True,
                    "post_state": {"client_build": "9990001", "session": {"charid": 90000001}},
                    "flight_recorder": {"macho_calls": [], "scatters_observed": [], "log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                }

            out_ok = root / "out_ok"
            rc_ok = self._run_with_step_hook(
                root,
                [scen],
                out_ok,
                _ok_step,
                init_client_logs=lambda rd, _cl: (
                    (rd / "client.log").write_text(benign_evomu_blocks, encoding="utf-8"),
                    (rd / "client_tracebacks.log").write_text(benign_evomu_blocks.replace("[evomu_site] ", ""), encoding="utf-8"),
                ),
            )
            self.assertEqual(rc_ok, 0)
            res_ok = json.loads((out_ok / "result.json").read_text(encoding="utf-8"))
            self.assertTrue(res_ok["journeys"]["evomu_site_logs"]["passed"], res_ok)

            out_bad_clog = root / "out_bad_clog"
            rc_bad_clog = self._run_with_step_hook(
                root,
                [scen],
                out_bad_clog,
                _ok_step,
                init_client_logs=lambda rd, _cl: (rd / "client.log").write_text(
                    benign_evomu_blocks + unhandled_evomu_client_log, encoding="utf-8"
                ),
            )
            self.assertEqual(rc_bad_clog, 1)
            res_bad_clog = json.loads((out_bad_clog / "result.json").read_text(encoding="utf-8"))
            self.assertEqual(res_bad_clog["journeys"]["evomu_site_logs"]["failure"]["failed_check"], "no_client_exception")

            out_bad_tblog = root / "out_bad_tblog"
            rc_bad_tblog = self._run_with_step_hook(
                root,
                [scen],
                out_bad_tblog,
                _ok_step,
                init_client_logs=lambda rd, _cl: (
                    (rd / "client.log").write_text(benign_evomu_blocks, encoding="utf-8"),
                    (rd / "client_tracebacks.log").write_text(unhandled_evomu_tb_log, encoding="utf-8"),
                ),
            )
            self.assertEqual(rc_bad_tblog, 1)
            res_bad_tblog = json.loads((out_bad_tblog / "result.json").read_text(encoding="utf-8"))
            self.assertEqual(res_bad_tblog["journeys"]["evomu_site_logs"]["failure"]["failed_check"], "no_client_exception")

    def test_scenario_runner_writes_non_gm_player_server_config_and_propagates_time_control_flag(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen = root / "non_gm.json"
            scen.write_text(
                json.dumps({
                    "id": "non_gm",
                    "starting_state": {"account": "capsuleer_one", "password": "pw1", "char_id": 91000011},
                    "steps": [
                        {"id": "undock_step", "action": {"type": "undock"}},
                        {"id": "tc_step", "time_control": True, "setup": True, "action": {"type": "wait", "wait_ms": 10}},
                    ],
                }),
                encoding="utf-8",
            )
            args = _fake_scenario_args(root, [scen], root / "out")
            runner = playtester.ScenarioRunner(args)
            runner.current_run_dir = root / "run_cfg"
            runner.current_run_dir.mkdir(parents=True, exist_ok=True)
            runner._active_starting_state = {"account": "capsuleer_one", "password": "pw1", "char_id": 91000011}
            runner._active_declared_setup_steps = []

            cfg_player = runner.write_server_config().read_text(encoding="utf-8")
            self.assertIn("gm_mode = false", cfg_player)
            self.assertNotIn("[[admin_accounts]]", cfg_player)
            self.assertIn("[[player_accounts]]", cfg_player)
            self.assertIn('username = "capsuleer_one"', cfg_player)
            self.assertIn('password_token = "pw1"', cfg_player)
            self.assertIn("[player_accounts.starter_character]", cfg_player)

            inclient_cfg = json.loads(runner.write_inclient_cfg().read_text(encoding="utf-8"))
            self.assertTrue(inclient_cfg.get("scenario_mode"))
            self.assertEqual(inclient_cfg.get("user"), "capsuleer_one")

            runner._active_declared_setup_steps = [{"step_index": 2, "step_id": "tc_step", "kind": "time_control"}]
            cfg_setup = runner.write_server_config().read_text(encoding="utf-8")
            self.assertIn("gm_mode = true", cfg_setup)
            self.assertIn("[[admin_accounts]]", cfg_setup)

            seen_payloads = []

            def _capture_payload(_runner, cmd, params):
                if cmd == "execute_action":
                    seen_payloads.append(dict(params.get("action") or {}))
                return {
                    "action_ok": True,
                    "post_state": {"client_build": "9990001", "session": {"charid": 91000011}},
                    "flight_recorder": {"macho_calls": [], "scatters_observed": [], "log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                }

            rc = self._run_with_step_hook(root, [scen], root / "out_flags", _capture_payload)
            self.assertEqual(rc, 0)
            self.assertEqual(len(seen_payloads), 2)
            self.assertTrue(seen_payloads[0].get("scenario_mode"))
            self.assertFalse(seen_payloads[0].get("time_control"))
            self.assertTrue(seen_payloads[1].get("scenario_mode"))
            # BF-1242-02: player session stays role = 0 so execute_action never enables client-side time_control
            self.assertFalse(seen_payloads[1].get("time_control"))

    def test_mid_run_per_step_log_offsets_fail_at_offending_step(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen = root / "three_steps.json"
            scen.write_text(
                json.dumps({
                    "id": "three_steps",
                    "steps": [
                        {"id": "step_one", "action": {"type": "wait", "target": "10"}, "checks": {"no_client_exception": True}},
                        {"id": "step_two", "action": {"type": "wait", "target": "20"}, "checks": {"no_client_exception": True}},
                        {"id": "step_three", "action": {"type": "wait", "target": "30"}, "checks": {"no_client_exception": True}},
                    ],
                }),
                encoding="utf-8",
            )

            seen_targets_tb = []

            def _tb_on_step2(runner, cmd, params):
                if cmd != "execute_action":
                    return {"macho_calls": []}
                tgt = str((params.get("action") or {}).get("target") or "")
                seen_targets_tb.append(tgt)
                if tgt == "20":
                    tb_path = runner.current_run_dir / "client_tracebacks.log"
                    tb_path.write_text(
                        "EXCEPTION #9 logged at 11/16/2026 00:00:05 : error\n"
                        "Traceback (most recent call last):\n"
                        '  File "eve/client/script/ui/shared/boom.py", line 55, in OnClick\n'
                        "RuntimeError: mid_step_boom\n"
                        "EXCEPTION END\n",
                        encoding="utf-8",
                    )
                return {
                    "action_ok": True,
                    "post_state": {"client_build": "9990001", "session": {"charid": 90000001}},
                    "flight_recorder": {"macho_calls": [], "scatters_observed": [], "log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                }

            out_tb = root / "out_step2_tb"
            rc_tb = self._run_with_step_hook(root, [scen], out_tb, _tb_on_step2)
            self.assertEqual(rc_tb, 1)
            self.assertEqual(seen_targets_tb, ["10", "20"])
            res_tb = json.loads((out_tb / "result.json").read_text(encoding="utf-8"))
            self.assertEqual(res_tb["journeys"]["three_steps"]["failure"]["step_index"], 2)
            self.assertEqual(res_tb["journeys"]["three_steps"]["failure"]["step_id"], "step_two")
            self.assertEqual(res_tb["journeys"]["three_steps"]["failure"]["failed_check"], "no_client_exception")

            seen_targets_ds = []

            def _desync_on_step2(runner, cmd, params):
                if cmd != "execute_action":
                    return {"macho_calls": []}
                tgt = str((params.get("action") or {}).get("target") or "")
                seen_targets_ds.append(tgt)
                if tgt == "20":
                    clog = runner.current_run_dir / "client_logs" / "eve_client.log"
                    with clog.open("a", encoding="utf-8") as f:
                        f.write("Michelle discarded a state that was too old (tick=12345)\n")
                return {
                    "action_ok": True,
                    "post_state": {"client_build": "9990001", "session": {"charid": 90000001}},
                    "flight_recorder": {"macho_calls": [], "scatters_observed": [], "log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                }

            out_ds = root / "out_step2_ds"
            rc_ds = self._run_with_step_hook(root, [scen], out_ds, _desync_on_step2)
            self.assertEqual(rc_ds, 1)
            self.assertEqual(seen_targets_ds, ["10", "20"])
            res_ds = json.loads((out_ds / "result.json").read_text(encoding="utf-8"))
            self.assertEqual(res_ds["journeys"]["three_steps"]["failure"]["step_index"], 2)
            self.assertEqual(res_ds["journeys"]["three_steps"]["failure"]["step_id"], "step_two")
            self.assertEqual(res_ds["journeys"]["three_steps"]["failure"]["failed_check"], "destiny_desync")


class TestRound4RpcAndAccountFixes(unittest.TestCase):
    @staticmethod
    def _load_hook_with_py2_stubs():
        import builtins
        import importlib.util
        import types

        hook_path = HARNESS_DIR / "evomu_playtester_hook.py"
        orig_builtin = sys.modules.get("__builtin__")
        orig_helpers = sys.modules.get("evomu_inclient_helpers")
        sys.modules["__builtin__"] = builtins
        fake_helpers = types.ModuleType("evomu_inclient_helpers")
        fake_helpers._wait_for_session = lambda *_a, **_kw: 90000001
        fake_helpers._sleep_ms = lambda _ms: None
        sys.modules["evomu_inclient_helpers"] = fake_helpers
        try:
            spec = importlib.util.spec_from_file_location("_evomu_playtester_hook_r4_test", str(hook_path))
            mod = importlib.util.module_from_spec(spec)
            assert spec is not None and spec.loader is not None
            mod.__dict__.setdefault("unicode", type("_Py2UnicodeStub", (), {}))
            mod.__dict__.setdefault("basestring", str)
            spec.loader.exec_module(mod)
            return mod
        finally:
            if orig_builtin is not None:
                sys.modules["__builtin__"] = orig_builtin
            else:
                sys.modules.pop("__builtin__", None)
            if orig_helpers is not None:
                sys.modules["evomu_inclient_helpers"] = orig_helpers
            else:
                sys.modules.pop("evomu_inclient_helpers", None)

    def test_validation_rejects_gm_slash_in_slash_cmd_without_setup_flag(self):
        bad_scen = {
            "id": "slash_cmd_gm_reject",
            "title": "Reject /giveisk hidden in slash_cmd",
            "steps": [
                {
                    "id": "sneak_gm",
                    "action": {
                        "type": "type_text",
                        "target": "chat_input",
                        "slash_cmd": "/giveisk 5000000",
                        "submit": True,
                    },
                }
            ],
        }
        with self.assertRaises(playtester.ScenarioValidationError) as ctx:
            playtester.validate_scenario(bad_scen)
        self.assertIn("/giveisk", str(ctx.exception).lower())

        self.assertEqual(
            playtester._is_gm_or_time_control_action({"type": "type_text", "slash_cmd": "/advancetime 60"})[:2],
            (False, True),
        )
        self.assertEqual(
            playtester._is_gm_or_time_control_action({"type": "type_text", "input": "/clearsessiontimer"})[:2],
            (True, False),
        )
        self.assertEqual(
            playtester._is_gm_or_time_control_action({"type": "type_text", "text": "/spawn 123"})[:2],
            (True, False),
        )
        self.assertEqual(
            playtester._is_gm_or_time_control_action({"type": "type_text", "slash_cmd": "o7 local"})[:2],
            (False, False),
        )

        setup_ok = {
            "id": "slash_cmd_gm_setup_ok",
            "title": "Declared setup step allowed",
            "steps": [
                {
                    "id": "setup_step",
                    "setup": True,
                    "action": {"type": "type_text", "target": "chat_input", "slash_cmd": "/giveisk 5000000", "submit": True},
                }
            ],
        }
        validated_setup = playtester.validate_scenario(setup_ok)
        self.assertEqual(len(validated_setup["declared_setup_steps"]), 1)

    def test_write_server_config_non_demo_defaults_and_zero_preservation(self):
        import tomllib
        import types

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen = root / "s.json"
            scen.write_text(
                json.dumps({"id": "s", "steps": [{"id": "w", "action": {"type": "wait", "wait_ms": 10}}]}),
                encoding="utf-8",
            )
            args = _fake_scenario_args(root, [scen], root / "out")
            runner = playtester.ScenarioRunner(args)
            with mock.patch.object(
                runner,
                "_resolve_sde_and_quasar_paths",
                return_value=(root / "evomu_sde.sqlite", root / "quasar_descriptors.sqlite"),
            ):
                runner._active_starting_state = {
                    "account": "pilot_custom",
                    "password": "pw",
                    "char_id": 92000010,
                    "bloodline_id": 1,
                    "gender": 0,
                    "initial_balance_isk": 0,
                    "initial_balance_plex": 0,
                }
                cfg = tomllib.loads(runner.write_server_config().read_text(encoding="utf-8"))
                self.assertFalse(cfg["feature_flags"]["seed_demo"])
                self.assertFalse(cfg["feature_flags"]["gm_mode"])
                self.assertNotIn("admin_accounts", cfg)
                self.assertEqual(cfg["player_accounts"][0]["username"], "pilot_custom")
                starter = cfg["player_accounts"][0]["starter_character"]
                self.assertEqual(starter["bloodline_id"], 1)
                self.assertEqual(starter["gender"], 0)
                self.assertEqual(starter["ancestry_id"], 11)
                self.assertEqual(starter["school_id"], 17)
                self.assertEqual(starter["initial_balance_isk"], 0)
                self.assertEqual(starter["initial_balance_plex"], 0)

                runner._active_starting_state = {"account": "pilot_amarr", "bloodline_id": 5}
                cfg_amarr = tomllib.loads(runner.write_server_config().read_text(encoding="utf-8"))
                self.assertEqual(cfg_amarr["player_accounts"][0]["starter_character"]["ancestry_id"], 1)
                self.assertEqual(cfg_amarr["player_accounts"][0]["starter_character"]["school_id"], 11)

                runner._active_starting_state = {"account": "demo"}
                cfg_demo = tomllib.loads(runner.write_server_config().read_text(encoding="utf-8"))
                self.assertTrue(cfg_demo["feature_flags"]["seed_demo"])

            created_calls = []

            class _FakeUnboundMgr:
                def GetCharactersToSelect(self):
                    return []

                def CreateCharacterWithDoll(self, name, bloodline_id, gender, ancestry_id, appearance, portrait, school_id):
                    created_calls.append((name, bloodline_id, gender, ancestry_id, school_id))

            fake_bridge = types.ModuleType("evomu_bridge")

            def _install_hook(sm, host="127.0.0.1", port=26000):
                sm.RemoteSvc = lambda svc: _FakeUnboundMgr() if svc == "charUnboundMgr" else mock.MagicMock()
                return mock.MagicMock()

            fake_bridge.install_service_manager_hook = _install_hook
            runner._active_starting_state = {"account": "pilot_custom"}
            with mock.patch.dict(sys.modules, {"evomu_bridge": fake_bridge}):
                names = runner._provision_extra_characters([
                    {"name": "Alt Caldari"},
                    {"name": "Alt Gallente", "bloodline_id": 7, "gender": 1},
                ])
            self.assertEqual(names, ["Alt Caldari", "Alt Gallente"])
            self.assertEqual(
                created_calls,
                [
                    ("Alt Caldari", 1, 0, 11, 17),
                    ("Alt Gallente", 7, 1, 13, 20),
                ],
            )

    def test_loopback_tcp_bridge_and_dispatch_end_to_end(self):
        import os
        import socket
        import threading

        hook = self._load_hook_with_py2_stubs()
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            probe.bind(("127.0.0.1", 0))
            free_port = probe.getsockname()[1]
            probe.close()

            hook._FLIGHT_RECORDER = [
                {"seq": 1, "service": "charUnboundMgr", "method": "GetCharactersToSelect", "ok": True},
            ]
            hook._RECENT_SCATTERS = [{"seq": 1, "event": "OnClientReady"}]

            live_state = {
                "client_build": "2841290",
                "client_version": "24.02",
                "active_view": "charsel",
                "session": {"charid": None, "solarsystemid2": 30000142},
                "open_windows": [{"window_id": "CharacterSelectionWindow", "name": "CharacterSelectionWindow"}],
                "modals": [],
                "ui": {"clickable_widgets": [{"name": "EnterGameBtn", "role": "button"}]},
            }

            def _fake_snapshot(_sm):
                return json.loads(json.dumps(live_state))

            def _fake_exec(_sm, act):
                if act.get("type") == "click_widget":
                    live_state["active_view"] = "station"
                    live_state["session"]["charid"] = 90000001
                    live_state["open_windows"] = [{"window_id": "InventoryWindow", "name": "InventoryWindow"}]
                    return {
                        "ok": True,
                        "action_ok": True,
                        "action": act,
                        "post_state": json.loads(json.dumps(live_state)),
                        "flight_recorder": {
                            "macho_calls": [{"seq": 2, "service": "charUnboundMgr", "method": "SelectCharacterID", "ok": True}],
                            "scatters_observed": ["OnSessionChanged"],
                            "log_exceptions": [],
                            "quasar_errors": [],
                            "macho_exceptions": [],
                        },
                    }
                return {
                    "ok": True,
                    "action_ok": True,
                    "action": act,
                    "flight_recorder": {
                        "macho_calls": [],
                        "scatters_observed": [],
                        "log_exceptions": [],
                        "quasar_errors": [],
                        "macho_exceptions": [],
                    },
                }

            stop_flag = threading.Event()
            with mock.patch.dict(os.environ, {"EVOMU_PLAYTESTER_PORT": ""}, clear=False), \
                 mock.patch.object(hook, "_ensure_windowed_mode", lambda *_a, **_kw: None), \
                 mock.patch.object(hook, "_install_flight_recorder_hooks", lambda *_a, **_kw: None), \
                 mock.patch.object(hook, "_install_warp_telemetry", lambda *_a, **_kw: None), \
                 mock.patch.object(hook, "_get_state_snapshot", _fake_snapshot), \
                 mock.patch.object(hook, "_execute_action", _fake_exec):
                srv = hook.PlaytesterRpcServer(
                    {"playtester_port": free_port, "run_dir": str(root), "scenario_mode": True},
                    mock.MagicMock(),
                )
                srv.sock.settimeout(0.1)

                def _serve_loop():
                    while not stop_flag.is_set() and srv.running:
                        try:
                            conn, _ = srv.sock.accept()
                        except socket.timeout:
                            continue
                        except OSError:
                            break
                        try:
                            conn.setblocking(1)
                            conn.settimeout(5.0)
                            rfile = conn.makefile("r", encoding="utf-8")
                            line = rfile.readline()
                            if line:
                                req = json.loads(line)
                                resp = srv._dispatch(req)
                                conn.sendall((json.dumps(hook._json_safe(resp)) + "\n").encode("utf-8"))
                            rfile.close()
                        finally:
                            conn.close()

                t = threading.Thread(target=_serve_loop, daemon=True)
                t.start()
                try:
                    bridge = playtester.InClientRpcBridge("127.0.0.1", srv.port, timeout=5.0)
                    boot = bridge.call("bootstrap_session", {"timeout_ms": 1000})
                    self.assertEqual(boot["client_build"], "2841290")
                    self.assertEqual(boot["active_view"], "charsel")

                    fr = bridge.call("get_flight_recorder", {"since_seq": 0})
                    self.assertEqual(len(fr["macho_calls"]), 1)
                    self.assertEqual(fr["macho_calls"][0]["method"], "GetCharactersToSelect")
                    self.assertEqual(fr["scatters_observed"], ["OnClientReady"])

                    st_now = bridge.call("get_state")
                    self.assertEqual(st_now["open_windows"][0]["window_id"], "CharacterSelectionWindow")

                    scen_path = root / "tcp_launch.json"
                    scen_path.write_text(
                        json.dumps({
                            "id": "tcp_launch",
                            "title": "TCP Launch E2E",
                            "steps": [
                                {
                                    "id": "verify_startup_charsel",
                                    "action": {"type": "wait", "wait_ms": 10},
                                    "checks": {
                                        "include_startup_rpcs": True,
                                        "rpc_sent": ["charUnboundMgr.GetCharactersToSelect"],
                                        "state_checks": [{"field": "active_view", "equals": "charsel"}],
                                    },
                                },
                                {
                                    "id": "enter_station",
                                    "action": {"type": "click_widget", "target": "EnterGameBtn"},
                                    "checks": {
                                        "expect_window_open": "InventoryWindow",
                                        "expect_session_field": "charid==90000001",
                                        "state_checks": [{"field": "active_view", "equals": "station"}],
                                    },
                                },
                            ],
                        }),
                        encoding="utf-8",
                    )

                    out_dir = root / "out_tcp"
                    args = _fake_scenario_args(root, [scen_path], out_dir)
                    runner = playtester.ScenarioRunner(args)

                    def _fake_start_server(reset_db=False):
                        runner.current_run_dir.mkdir(parents=True, exist_ok=True)
                        (runner.current_run_dir / "server.log").write_text(
                            "2026-11-16T00:00:00Z INFO Listening on Native machoNet / GPSTransport TCP\n",
                            encoding="utf-8",
                        )
                        return True

                    def _fake_start_client():
                        with (runner.current_run_dir / "server.log").open("a", encoding="utf-8") as sf:
                            sf.write("2026-11-16T00:00:01Z INFO Accepted native machoNet connection peer=127.0.0.1:50000\n")
                            sf.write("2026-11-16T00:00:01Z INFO Dispatching RPC call user_id=Some(1) service=charUnboundMgr method=GetCharacterSelectionData\n")
                        runner.rpc = playtester.InClientRpcBridge("127.0.0.1", srv.port, timeout=5.0)
                        runner._last_boot_result = runner.rpc.call("bootstrap_session", {"timeout_ms": 1000})
                        return True

                    with mock.patch.object(runner, "start_server", _fake_start_server), \
                         mock.patch.object(runner, "start_client", _fake_start_client), \
                         mock.patch.object(runner, "stop_client", lambda: None), \
                         mock.patch.object(runner, "stop_server", lambda: None):
                        rc = runner.run_scenarios()

                    self.assertEqual(rc, 0)
                    res = json.loads((out_dir / "result.json").read_text(encoding="utf-8"))
                    j = res["journeys"]["tcp_launch"]
                    self.assertTrue(j["passed"], res)
                    self.assertTrue(j["ui_scenario"])
                    self.assertEqual(j["client_build"], 2841290)
                    self.assertEqual(res["client_build"], 2841290)
                    self.assertIn("charUnboundMgr.GetCharactersToSelect", j["step_rpcs"]["verify_startup_charsel"])
                    self.assertEqual(len(j["steps"]), 2)
                    self.assertEqual(j["steps"][1]["step_id"], "enter_station")
                    self.assertTrue(j["steps"][1]["passed"])
                    self.assertEqual(bridge.call("get_state")["session"]["charid"], 90000001)
                finally:
                    stop_flag.set()
                    srv.running = False
                    try:
                        srv.sock.close()
                    except OSError:
                        pass
                    t.join(timeout=2.0)


class Test1202R4And1242Acceptance(unittest.TestCase):
    def _runner_in_dir(self, root: Path, scen_paths: list) -> playtester.ScenarioRunner:
        wt = root / "wt"
        wt.mkdir(parents=True, exist_ok=True)
        args = playtester.parse_args(["--worktree", str(wt), "--no-hybrid"])
        return playtester.ScenarioRunner(args, scenario_paths=scen_paths, out_dir=root / "out")

    def test_p1_scenario_gm_never_clobbers_demo_or_starter_state(self):
        import tomllib

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            # Case A: custom account (`user != "demo"`) with setup=True
            scen_custom = root / "custom_setup.json"
            scen_custom.write_text(
                json.dumps({
                    "id": "custom_setup",
                    "starting_state": {
                        "account": "caldari_pilot",
                        "password": "secret",
                        "char_id": 91000011,
                        "character_name": "Kestrel Starter",
                        "bloodline_id": 1,
                        "gender": 0,
                        "initial_balance_isk": 5500000,
                    },
                    "steps": [
                        {"id": "s1", "setup": True, "action": {"type": "slash_command", "command": "/giveisk 100"}},
                    ],
                }),
                encoding="utf-8",
            )
            loaded_custom = playtester.load_scenario_file(scen_custom)
            runner_c = self._runner_in_dir(root / "c1", [scen_custom])
            runner_c._active_starting_state = loaded_custom["starting_state"]
            runner_c._active_declared_setup_steps = loaded_custom["declared_setup_steps"]
            cfg_c = tomllib.loads(runner_c.write_server_config().read_text(encoding="utf-8"))
            player_names = [a["username"] for a in cfg_c["player_accounts"]]
            admin_names = [a["username"] for a in cfg_c.get("admin_accounts", [])]
            self.assertEqual(player_names, ["caldari_pilot"])
            self.assertEqual(admin_names, ["evomu_scenario_gm"])
            self.assertNotIn("caldari_pilot", admin_names)
            self.assertEqual(cfg_c["player_accounts"][0]["starter_character"]["name"], "Kestrel Starter")
            self.assertEqual(cfg_c["player_accounts"][0]["starter_character"]["initial_balance_isk"], 5500000)

            # Case B: `demo` account with setup=True -> no [[admin_accounts]] in TOML, EVOMU_ADMIN_TOKEN env used instead
            scen_demo = root / "demo_setup.json"
            scen_demo.write_text(
                json.dumps({
                    "id": "demo_setup",
                    "starting_state": {"account": "demo", "char_id": 90000001},
                    "steps": [
                        {"id": "s1", "time_control": True, "action": {"type": "advance_server_time", "target": "60"}},
                    ],
                }),
                encoding="utf-8",
            )
            loaded_demo = playtester.load_scenario_file(scen_demo)
            runner_d = self._runner_in_dir(root / "c2", [scen_demo])
            runner_d._active_starting_state = loaded_demo["starting_state"]
            runner_d._active_declared_setup_steps = loaded_demo["declared_setup_steps"]
            cfg_d = tomllib.loads(runner_d.write_server_config().read_text(encoding="utf-8"))
            self.assertNotIn("admin_accounts", cfg_d)
            self.assertTrue(cfg_d["feature_flags"]["seed_demo"])
            self.assertIn("EVOMU_ADMIN_TOKEN", runner_d._scenario_server_extra_env())

    def test_charsel_listed_all_characters_requires_primary_with_extra_characters(self):
        post_only_extra = {
            "charsel_state": {"active": True, "names": ["Aura Two"], "slot_count": 1},
            "session": {},
        }
        ok_bad, chk_bad, _ = playtester.evaluate_scenario_checks(
            {"charsel_listed_all_characters": True},
            {},
            post_only_extra,
            {"action_ok": True, "flight_recorder": {}},
            context={"primary_character_name": "Demo Pilot", "expected_characters": ["Aura Two"]},
        )
        self.assertFalse(ok_bad)
        self.assertEqual(chk_bad, "charsel_listed_all_characters")

        post_both = {
            "charsel_state": {"active": True, "names": ["Demo Pilot", "Aura Two"], "slot_count": 2},
            "session": {},
        }
        ok_good, _, _ = playtester.evaluate_scenario_checks(
            {"charsel_listed_all_characters": True},
            {},
            post_both,
            {"action_ok": True, "flight_recorder": {}},
            context={"primary_character_name": "Demo Pilot", "expected_characters": ["Aura Two"]},
        )
        self.assertTrue(ok_good)

    def test_ui_delta_without_relog_sqlite_parity_pass_and_timeout_fail(self):
        import sqlite3

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen_path = root / "ui_delta.json"
            scen_path.write_text(
                json.dumps({
                    "id": "ui_delta_check",
                    "starting_state": {"account": "demo"},
                    "steps": [
                        {
                            "id": "buy_widget",
                            "action": {"type": "click_widget", "target": "BuyBtn"},
                            "checks": {"ui_delta_without_relog": True},
                        }
                    ],
                }),
                encoding="utf-8",
            )
            runner = self._runner_in_dir(root, [scen_path])
            runner.world_dir.mkdir(parents=True, exist_ok=True)
            with sqlite3.connect(str(runner.world_db_path)) as conn:
                conn.execute("CREATE TABLE characters (character_id INTEGER PRIMARY KEY, user_id INTEGER, name TEXT, corporation_id INTEGER, alliance_id INTEGER, station_id INTEGER, structure_id INTEGER, solar_system_id INTEGER, active_ship_id INTEGER, balance_cents INTEGER, activity_status TEXT, version INTEGER, record_json TEXT)")
                conn.execute("CREATE TABLE items (item_id INTEGER PRIMARY KEY, type_id INTEGER, owner_id INTEGER, location_id INTEGER, flag INTEGER, quantity INTEGER, singleton INTEGER, version INTEGER, record_json TEXT)")
                conn.execute("CREATE TABLE jump_clones (character_id INTEGER PRIMARY KEY, clones_json TEXT)")
                conn.execute("INSERT INTO characters VALUES (90000001, 1, 'Demo Pilot', 1000009, NULL, 60003760, NULL, 30000142, 100000001, 150000000, NULL, 1, '{\"home_station_id\": 60003760}')")
                conn.execute("INSERT INTO items VALUES (2001, 34, 90000001, 60003760, 4, 500, 0, 1, '{}')")
                conn.execute("INSERT INTO jump_clones VALUES (90000001, '[{\"clone_id\": 1}]')")
                conn.commit()

            get_state_polls = {"n": 0}

            def _fake_rpc_pass(method, params=None, timeout=15.0):
                if method == "execute_action":
                    return {
                        "action_ok": True,
                        "post_state": {
                            "client_build": "3698585",
                            "session": {"charid": 90000001, "stationid": 60003760, "shipid": 100000001},
                            "wallet_state": {"isk_balance": 1000000.0},
                            "inventory_state": {"hangar_item_count": 0, "hangar_total_quantity": 0},
                            "clone_state": {"home_station_id": 60003760, "jump_clone_count": 1},
                        },
                        "flight_recorder": {"macho_calls": []},
                    }
                if method == "get_state":
                    get_state_polls["n"] += 1
                    return {
                        "client_build": "3698585",
                        "session": {"charid": 90000001, "stationid": 60003760, "shipid": 100000001},
                        "wallet_state": {"isk_balance": 1500000.0},
                        "inventory_state": {"hangar_item_count": 1, "hangar_total_quantity": 500},
                        "clone_state": {"home_station_id": 60003760, "jump_clone_count": 1},
                    }
                return {"ok": True}

            boot_snap = {"ok": True, "state": {"client_build": "3698585", "client_version": "24.02", "session": {"charid": 90000001}}, "flight_recorder": {"macho_calls": []}}
            fake_rpc = mock.Mock()
            fake_rpc.call.side_effect = _fake_rpc_pass
            runner.rpc = fake_rpc
            runner.start_server = lambda reset_db=True: True
            def _boot():
                runner._last_boot_result = boot_snap
                return True
            runner.start_client = _boot
            runner.stop_client = lambda: None
            runner.stop_server = lambda: None
            with mock.patch.object(playtester, "get_git_head", return_value="abc1234"), \
                 mock.patch.object(runner, "_run_destiny_parity_check", return_value=None):
                rc_pass = runner.run_scenarios()
            self.assertEqual(rc_pass, 0)
            self.assertGreaterEqual(get_state_polls["n"], 1)

            # Now simulate permanent desync (wallet remains stale) -> fails cleanly after 2s window
            def _fake_rpc_fail(method, params=None, timeout=15.0):
                return {
                    "action_ok": True,
                    "client_build": "3698585",
                    "session": {"charid": 90000001, "stationid": 60003760},
                    "wallet_state": {"isk_balance": 999.0},
                    "post_state": {
                        "client_build": "3698585",
                        "session": {"charid": 90000001, "stationid": 60003760},
                        "wallet_state": {"isk_balance": 999.0},
                    },
                    "flight_recorder": {"macho_calls": []},
                }

            fake_rpc.call.side_effect = _fake_rpc_fail
            rc_fail = runner.run_scenarios()
            self.assertEqual(rc_fail, 1)
            fail_res = json.loads((root / "out" / "result.json").read_text(encoding="utf-8"))
            self.assertEqual(fail_res["journeys"]["ui_delta_check"]["failure"]["failed_check"], "ui_delta_without_relog")

    def test_window_sweep_scenario_validates_against_hook_cmd_map(self):
        sweep_path = playtester.SCENARIOS_DIR / "window_sweep.json"
        self.assertTrue(sweep_path.exists(), f"Missing {sweep_path}")
        loaded = playtester.load_scenario_file(sweep_path)
        self.assertEqual(loaded["id"], "window_and_showinfo_sweep")
        self.assertEqual(loaded["declared_setup_steps"], [])

        cmd_map, guards = playtester.load_hook_open_window_tables()
        opened_targets = [
            playtester._normalize_window_target_key(st["action"].get("target", ""))
            for st in loaded["steps"]
            if st["action"].get("type") == "open_window"
        ]
        for tgt in opened_targets:
            self.assertIn(tgt, cmd_map)
        expected_cmds = {
            mname
            for k, mname in cmd_map.items()
            if guards.get(k) != "structure_control_only"
        }
        covered_cmds = {cmd_map[t] for t in opened_targets}
        self.assertEqual(covered_cmds, expected_cmds)

        showinfo_cats = [
            st["action"].get("category")
            for st in loaded["steps"]
            if st["action"].get("type") == "show_info"
        ]
        self.assertEqual(
            set(showinfo_cats),
            {"ship", "turret", "blueprint", "skillbook", "implant", "structure", "filament", "pi_pin", "skin", "corpse"},
        )

        # Reject invalid open_window target
        with tempfile.TemporaryDirectory() as td:
            bad = Path(td) / "bad_win.json"
            bad.write_text(
                json.dumps({
                    "id": "bad_win",
                    "steps": [{"id": "s1", "action": {"type": "open_window", "target": "nonexistent_bogus_window"}}],
                }),
                encoding="utf-8",
            )
            with self.assertRaises(playtester.ScenarioValidationError):
                playtester.load_scenario_file(bad)

        undock_step = next(st for st in loaded["steps"] if st["id"] == "undock_for_space_windows")
        self.assertEqual(undock_step["checks"]["state_checks"], [{"field": "active_view", "equals": "inflight"}])
        self.assertEqual(undock_step["checks"]["ship_mode"], [0, 1, 2, 3, 4])
        self.assertEqual(loaded["steps"][-1]["id"], "dock_back_jita")
        self.assertEqual(loaded["steps"][-1]["action"]["type"], "dock")
        open_steps = [st for st in loaded["steps"] if st["action"].get("type") == "open_window"]
        self.assertEqual(len(open_steps), 56)
        self.assertTrue(all(st["action"].get("require_window_open") is True for st in open_steps))
        si_steps = [st for st in loaded["steps"] if st["action"].get("type") == "show_info"]
        self.assertEqual(len(si_steps), 10)
        self.assertTrue(all(st["checks"].get("expect_window_open") == "info" for st in si_steps))

    def test_validate_scenario_rejects_string_or_bool_ship_mode(self):
        for bad_mode in ("space", True, [], [1, "space"]):
            with self.assertRaises(playtester.ScenarioValidationError):
                playtester.validate_scenario({
                    "id": "bad_sm",
                    "steps": [{
                        "id": "s1",
                        "action": {"type": "undock"},
                        "checks": {"ship_mode": bad_mode},
                    }],
                })
        ok_int = playtester.validate_scenario({
            "id": "ok_sm_int",
            "steps": [{"id": "s1", "action": {"type": "undock"}, "checks": {"ship_mode": 1}}],
        })
        self.assertEqual(ok_int["steps"][0]["checks"]["ship_mode"], 1)
        ok_list = playtester.validate_scenario({
            "id": "ok_sm_list",
            "steps": [{"id": "s1", "action": {"type": "undock"}, "checks": {"ship_mode": [0, 1, 2, 3, 4]}}],
        })
        self.assertEqual(ok_list["steps"][0]["checks"]["ship_mode"], [0, 1, 2, 3, 4])

    def test_ui_delta_sqlite_parity_reconciles_station_items_and_station_ships(self):
        import sqlite3

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            world_db = root / "world.sqlite"
            sde_db = root / "sde.sqlite"
            sde_conn = sqlite3.connect(str(sde_db))
            try:
                sde_conn.execute("CREATE TABLE sde_types (type_id INTEGER PRIMARY KEY, category_id INTEGER)")
                sde_conn.execute("INSERT INTO sde_types VALUES (34, 4)")
                sde_conn.execute("INSERT INTO sde_types VALUES (587, 6)")
                sde_conn.commit()
            finally:
                sde_conn.close()
            w_conn = sqlite3.connect(str(world_db))
            try:
                w_conn.execute("CREATE TABLE characters (character_id INTEGER PRIMARY KEY, station_id INTEGER, structure_id INTEGER, active_ship_id INTEGER, balance_cents INTEGER, record_json TEXT)")
                w_conn.execute("CREATE TABLE items (item_id INTEGER PRIMARY KEY, type_id INTEGER, owner_id INTEGER, location_id INTEGER, flag INTEGER, quantity INTEGER, singleton INTEGER, version INTEGER, record_json TEXT)")
                w_conn.execute("INSERT INTO characters VALUES (90000001, 60003760, NULL, 100000001, 10000000, '{}')")
                w_conn.execute("INSERT INTO items VALUES (100000001, 587, 90000001, 60003760, 4, 1, 1, 1, '{}')")
                w_conn.execute("INSERT INTO items VALUES (2001, 34, 90000001, 60003760, 4, 500, 0, 1, '{}')")
                w_conn.commit()
            finally:
                w_conn.close()

            # Client only cached StationItems (10004, 4): 1 non-ship qty 500, 0 ships
            ok_items, msg_items = playtester.evaluate_ui_delta_sqlite_parity(
                world_db,
                {
                    "session": {"charid": 90000001, "stationid": 60003760},
                    "inventory_state": {
                        "hangar_containers_seen": ["StationItems"],
                        "hangar_item_count": 1,
                        "hangar_total_quantity": 500,
                        "hangar_non_ship_count": 1,
                        "hangar_non_ship_total_quantity": 500,
                        "ship_hangar_count": 0,
                        "ship_hangar_total_quantity": 0,
                    },
                },
                sde_path=sde_db,
            )
            self.assertTrue(ok_items, msg_items)

            # Client cached unified StationItems + StationHangar: 1 non-ship + 1 ship
            ok_both, msg_both = playtester.evaluate_ui_delta_sqlite_parity(
                world_db,
                {
                    "session": {"charid": 90000001, "stationid": 60003760},
                    "inventory_state": {
                        "hangar_containers_seen": ["StationHangar", "StationItems"],
                        "hangar_item_count": 2,
                        "hangar_total_quantity": 501,
                        "hangar_non_ship_count": 1,
                        "hangar_non_ship_total_quantity": 500,
                        "ship_hangar_count": 1,
                        "ship_hangar_total_quantity": 1,
                    },
                },
                sde_path=sde_db,
            )
            self.assertTrue(ok_both, msg_both)

            # StationHangar WAS loaded but ship disappeared -> must fail
            ok_miss_ship, msg_miss_ship = playtester.evaluate_ui_delta_sqlite_parity(
                world_db,
                {
                    "session": {"charid": 90000001, "stationid": 60003760},
                    "inventory_state": {
                        "hangar_containers_seen": ["StationHangar", "StationItems"],
                        "hangar_item_count": 1,
                        "hangar_total_quantity": 500,
                        "hangar_non_ship_count": 1,
                        "hangar_non_ship_total_quantity": 500,
                        "ship_hangar_count": 0,
                        "ship_hangar_total_quantity": 0,
                    },
                },
                sde_path=sde_db,
            )
            self.assertFalse(ok_miss_ship)
            self.assertIn("ship_hangar_count", msg_miss_ship)

            # StationItems WAS loaded but non-ship item disappeared -> must fail
            ok_miss_item, msg_miss_item = playtester.evaluate_ui_delta_sqlite_parity(
                world_db,
                {
                    "session": {"charid": 90000001, "stationid": 60003760},
                    "inventory_state": {
                        "hangar_containers_seen": ["StationItems"],
                        "hangar_item_count": 0,
                        "hangar_total_quantity": 0,
                        "hangar_non_ship_count": 0,
                        "hangar_non_ship_total_quantity": 0,
                        "ship_hangar_count": 0,
                    },
                },
                sde_path=sde_db,
            )
            self.assertFalse(ok_miss_item)
            self.assertIn("hangar_non_ship_count", msg_miss_item)

    def test_scenario_gm_and_time_control_route_via_gm_bridge_keeping_player_role_zero(self):
        import types

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            for label, acct, exp_gm_user, exp_gm_cid, target_cid in (
                ("custom", "caldari_pilot", "evomu_scenario_gm", 90000099, 91000011),
                ("demo", "demo", "admin", 90000003, 90000001),
            ):
                scen_path = root / f"{label}_gm_bridge.json"
                scen_path.write_text(
                    json.dumps({
                        "id": f"{label}_gm_bridge",
                        "starting_state": {"account": acct, "password": "pw", "char_id": target_cid},
                        "steps": [
                            {
                                "id": "gm_giveisk",
                                "setup": True,
                                "action": {"type": "slash_command", "command": "/giveisk 5000000"},
                                "checks": {"rpc_sent": ["slash.SlashCmd"]},
                            },
                            {
                                "id": "gm_advance",
                                "time_control": True,
                                "action": {"type": "advance_server_time", "target": "60"},
                                "checks": {"rpc_sent": ["timeControl.AdvanceClock"]},
                            },
                            {
                                "id": "player_undock",
                                "time_control": True,
                                "action": {"type": "undock"},
                                "wait_until": {"timeout_ms": 5000, "poll_interval_ms": 250, "checks": {"state_checks": [{"field": "active_view", "equals": "inflight"}]}},
                                "checks": {"state_checks": [{"field": "active_view", "equals": "inflight"}]},
                            },
                        ],
                    }),
                    encoding="utf-8",
                )
                runner = self._runner_in_dir(root / label, [scen_path])
                gm_events = []
                player_exec_actions = []
                view_state = {"view": "hangar", "poll": 0}

                class _FakeGmConn:
                    def __init__(self):
                        self.connected = True
                    def close(self):
                        self.connected = False
                        gm_events.append(("close",))

                class _FakeSvc:
                    def __init__(self, svc_name):
                        self.svc_name = svc_name
                    def Login(self, u, pw):
                        gm_events.append(("Login", u, pw))
                        return True
                    def GetCharactersToSelect(self):
                        return [{"characterID": exp_gm_cid}]
                    def SelectCharacterID(self, cid):
                        gm_events.append(("SelectCharacterID", cid))
                        return True
                    def SlashCmd(self, cmd):
                        gm_events.append(("SlashCmd", cmd))
                        return {"ok": True}
                    def AdvanceClock(self, secs):
                        gm_events.append(("AdvanceClock", secs))
                        return True
                    def ClearSessionTimer(self):
                        gm_events.append(("ClearSessionTimer",))
                        return True

                fake_bridge = types.ModuleType("evomu_bridge")
                def _install_hook(sm, host="127.0.0.1", port=26000):
                    sm.RemoteSvc = lambda name: _FakeSvc(name)
                    return _FakeGmConn()
                fake_bridge.install_service_manager_hook = _install_hook

                def _fake_rpc_call(method, params=None, timeout=15.0):
                    if method == "execute_action":
                        act = (params or {}).get("action") or {}
                        player_exec_actions.append(dict(act))
                        if act.get("type") == "wait":
                            view_state["poll"] += 1
                            if view_state["poll"] >= 1:
                                view_state["view"] = "inflight"
                        return {
                            "action_ok": True,
                            "post_state": {
                                "client_build": "3698585",
                                "active_view": view_state["view"],
                                "session": {"charid": target_cid, "solarsystemid2": 30000142},
                            },
                            "flight_recorder": {"macho_calls": []},
                        }
                    if method == "get_state":
                        return {
                            "client_build": "3698585",
                            "active_view": view_state["view"],
                            "session": {"charid": target_cid, "solarsystemid2": 30000142},
                        }
                    return {"ok": True}

                runner.rpc = mock.Mock()
                runner.rpc.call.side_effect = _fake_rpc_call
                runner.start_server = lambda reset_db=True: True
                def _boot():
                    runner._last_boot_result = {
                        "ok": True,
                        "state": {"client_build": "3698585", "active_view": "hangar", "session": {"charid": target_cid}},
                        "flight_recorder": {"macho_calls": []},
                    }
                    return True
                runner.start_client = _boot
                runner.stop_client = lambda: None
                runner.stop_server = lambda: None

                with mock.patch.dict(sys.modules, {"evomu_bridge": fake_bridge}), \
                     mock.patch.object(playtester, "get_git_head", return_value="abc1234"), \
                     mock.patch.object(runner, "_run_destiny_parity_check", return_value=None):
                    rc = runner.run_scenarios()
                self.assertEqual(rc, 0, gm_events)
                self.assertIn(("Login", exp_gm_user, "evomu-scenario-gm-secret"), gm_events)
                self.assertIn(("SelectCharacterID", exp_gm_cid), gm_events)
                self.assertIn(("SlashCmd", f"/giveisk {target_cid} 5000000"), gm_events)
                self.assertIn(("AdvanceClock", 60), gm_events)
                self.assertIn(("AdvanceClock", 12), gm_events)
                self.assertIn(("close",), gm_events)
                # Player session (role = 0) never receives slash_command / advance_server_time or time_control=True
                self.assertTrue(all(a.get("time_control") is False for a in player_exec_actions), player_exec_actions)
                self.assertFalse(any(a.get("type") in ("slash_command", "advance_server_time") for a in player_exec_actions))


if __name__ == "__main__":
    unittest.main()
