#!/usr/bin/env python3
"""Offline unit & acceptance tests for the EvoMu host playtester daemon (playtester.py)."""

import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib  # type: ignore[no-redef]
    except ImportError:
        tomllib = None  # type: ignore[assignment]
import types
import unittest
from pathlib import Path
from unittest import mock

HARNESS_DIR = Path(__file__).resolve().parent
if str(HARNESS_DIR) not in sys.path:
    sys.path.insert(0, str(HARNESS_DIR))

import playtester



class TestSchemasPromptsAndMemory(unittest.TestCase):
    def test_schemas_include_jump_gate(self):
        for schema_name in ("playtester_executor_schema.json", "playtester_action_schema.json"):
            data = json.loads((HARNESS_DIR / schema_name).read_text(encoding="utf-8"))
            action_enum = data["properties"]["action"]["properties"]["type"]["enum"]
            self.assertIn("jump_gate", action_enum, f"Missing jump_gate in {schema_name}")
            self.assertIn("type_text", action_enum)
            self.assertIn("press_key", action_enum)



    def test_prompts_document_stage1_features(self):
        with tempfile.TemporaryDirectory() as td:
            mem = playtester.PlaytesterMemory(Path(td), git_head="deadbeef")
            pp = playtester.build_planner_prompt(
                1, Path(td) / "screen.png", [], {"active_view": "space"}, {}, mem, "deadbeef"
            )
            self.assertIn("jump_gate", pp)
            self.assertIn("editable", pp)
            self.assertIn("ALT+P", pp)
            self.assertIn("double_click", pp)
            self.assertIn("action_type_hint", pp)


    def test_memory_normalize_prunes_orphans_maps_aliases_and_decays_on_rebuild(self):
        personas_cfg = {
            "personas": [
                {"id": "newbro_career_and_fitting", "title": "Career", "rotate_after_steps": 25, "goals": ["Fit ship"]},
                {"id": "mining_and_resource_harvesting", "title": "Mining", "rotate_after_steps": 25, "goals": ["Mine"]},
                {"id": "economy_market_contracts_and_industry", "title": "Economy", "rotate_after_steps": 25, "goals": ["Trade"]},
            ],
            "initial_coverage_frontier": [
                {"feature": "ui.skills.queue_and_training", "persona": "newbro_career_and_fitting", "status": "untested"},
                {"feature": "ui.fitting.equip_online_and_undock_verify", "persona": "newbro_career_and_fitting", "status": "untested"},
                {"feature": "space.mining.asteroid_lock_and_laser_cycle", "persona": "mining_and_resource_harvesting", "status": "untested"},
                {"feature": "ui.market.search_buy_and_cancel", "persona": "economy_market_contracts_and_industry", "status": "untested"},
            ],
        }
        with tempfile.TemporaryDirectory() as td:
            state_dir = Path(td)
            p_path = state_dir / "personas.json"
            p_path.write_text(json.dumps(personas_cfg), encoding="utf-8")
            raw_mem = {
                "created_at": "2026-11-09T00:00:00Z",
                "updated_at": "2026-11-10T00:00:00Z",
                "total_steps": 500,
                "server_reloads": 1,
                "last_server_git_head": "aaa1111",
                "last_server_bin_mtime": 1000.0,
                "active_persona_index": 0,
                "active_persona_steps": 5,
                "coverage_frontier": [
                    {"feature": "skills_window_and_queue", "persona": "", "status": "verified", "verified_at_step": 10},
                    {"feature": "ui.fitting.equip_and_online_modules", "persona": "newbro_career_and_fitting", "status": "verified", "verified_at_step": 40},
                    {"feature": "space.mining.asteroid_lock_and_laser_cycle", "persona": "mining_and_resource_harvesting", "status": "buggy", "buggy_at_step": 120},
                    {"feature": "ui.market.orders_and_history", "persona": "economy_market_contracts_and_industry", "status": "buggy", "buggy_at_step": 250},
                ] + [
                    {"feature": f"random_freeform_noise_{i}", "persona": "", "status": "untested"}
                    for i in range(50)
                ],
            }
            (state_dir / "memory.json").write_text(json.dumps(raw_mem), encoding="utf-8")

            mem = playtester.PlaytesterMemory(
                state_dir,
                git_head="aaa1111",
                server_bin_mtime=1000.0,
                personas_path=p_path,
            )
            by_feat = {x["feature"]: x for x in mem.data["coverage_frontier"]}
            self.assertEqual(by_feat["ui.skills.queue_and_training"]["status"], "verified")
            self.assertEqual(by_feat["ui.fitting.equip_online_and_undock_verify"]["status"], "verified")
            self.assertEqual(by_feat["space.mining.asteroid_lock_and_laser_cycle"]["status"], "buggy")
            self.assertEqual(by_feat["ui.market.search_buy_and_cancel"]["status"], "buggy")
            self.assertTrue(all(x.get("persona") for x in mem.data["coverage_frontier"]))
            self.assertEqual(len(mem.data["coverage_frontier"]), 4)

            stats = mem.normalize_coverage_frontier(git_head="bbb2222", server_bin_mtime=1050.0, save=True)
            self.assertTrue(stats["server_rebuilt"])
            self.assertEqual(stats["buggy_decayed"], 2)
            by_feat2 = {x["feature"]: x for x in mem.data["coverage_frontier"]}
            self.assertEqual(by_feat2["space.mining.asteroid_lock_and_laser_cycle"]["status"], "untested")
            self.assertTrue(by_feat2["space.mining.asteroid_lock_and_laser_cycle"]["needs_retest"])
            self.assertEqual(mem.data["last_server_git_head"], "bbb2222")

    def test_buggy_latch_fixed_and_custom_features_capped(self):
        personas_cfg = {
            "personas": [
                {"id": "mining_and_resource_harvesting", "title": "Mining", "rotate_after_steps": 25, "goals": ["Mine"]},
            ],
            "initial_coverage_frontier": [
                {"feature": "space.mining.asteroid_lock_and_laser_cycle", "persona": "mining_and_resource_harvesting", "status": "untested"},
            ],
        }
        with tempfile.TemporaryDirectory() as td:
            state_dir = Path(td)
            p_path = state_dir / "personas.json"
            p_path.write_text(json.dumps(personas_cfg), encoding="utf-8")
            mem = playtester.PlaytesterMemory(state_dir, git_head="deadbeef", personas_path=p_path)
            mem.apply_planner_decision(
                10,
                {
                    "current_goal": "Test mining",
                    "memory_update": {
                        "working_memory": ["Mining failed"],
                        "mark_tested_features": [],
                        "mark_buggy_features": ["space.mining.asteroid_lock_and_laser_cycle"],
                        "new_frontier_features": [],
                    },
                },
            )
            self.assertEqual(mem.data["coverage_frontier"][0]["status"], "buggy")
            mem.apply_planner_decision(
                20,
                {
                    "current_goal": "Verify mining fixed",
                    "memory_update": {
                        "working_memory": ["Mining succeeded"],
                        "mark_tested_features": ["space.mining.asteroid_lock_and_laser_cycle"],
                        "mark_buggy_features": [],
                        "new_frontier_features": [
                            "custom_slug_one",
                            "custom_slug_two",
                            "custom_slug_three",
                            "custom_slug_four",
                            "custom_slug_five_should_be_capped",
                            "this is a verbose prose sentence that should never enter frontier",
                        ],
                    },
                },
            )
            by_feat2 = {x["feature"]: x for x in mem.data["coverage_frontier"]}
            self.assertEqual(by_feat2["space.mining.asteroid_lock_and_laser_cycle"]["status"], "verified")
            self.assertEqual(by_feat2["space.mining.asteroid_lock_and_laser_cycle"]["verified_at_step"], 20)
            self.assertNotIn("custom_slug_five_should_be_capped", by_feat2)
            self.assertNotIn("this is a verbose prose sentence that should never enter frontier", by_feat2)
            self.assertIn("custom_slug_four", by_feat2)
            self.assertTrue(all(x.get("persona") for x in mem.data["coverage_frontier"]))


class TestBugOracleSlashErrors(unittest.TestCase):
    def _eval_slash_error(self, err_str: str, expected_text: str = "Execute slash command", log_excs=None):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            sdir = root / "step_1"
            qdir.mkdir()
            sdir.mkdir()
            shot = root / "shot.png"
            shot.write_bytes(b"png")
            tb = root / "tb.log"
            tb.write_text("", encoding="utf-8")
            srv = root / "srv.log"
            srv.write_text("", encoding="utf-8")
            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            decision = {
                "action": {
                    "type": "slash_command",
                    "command": "/badcmd",
                    "expected_ui_and_state_change": expected_text,
                },
                "bug_report": {"should_file": False},
            }
            exec_res = {
                "action_ok": False,
                "action_error": err_str,
                "flight_recorder": {
                    "log_exceptions": log_excs or [],
                    "quasar_errors": [],
                    "macho_exceptions": [{"service": "slash", "method": "SlashCmd", "error": err_str}],
                },
            }
            return oracle.evaluate_step(
                step_num=1,
                git_head="deadbeef",
                decision=decision,
                exec_res=exec_res,
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )

    def test_benign_slash_errors_with_reason_not_queued(self):
        self.assertEqual(
            self._eval_slash_error(
                "UserError('SlashError', {'reason': \"UnknownSlashCommand: Unknown or unsupported slash command '/help'\"})"
            ),
            [],
        )
        self.assertEqual(
            self._eval_slash_error(
                "UserError('SlashError', {'reason': 'Commands: [\"heal\", \"giveisk\"]'})"
            ),
            [],
        )

    def test_slash_error_without_reason_still_queued_even_on_negative_test(self):
        queued = self._eval_slash_error(
            "UserError('SlashError', {'msg': 'broken'})",
            expected_text="Expect UserError reject on invalid command",
            log_excs=[{"headline": "KeyError: 'reason'", "trace": "KeyError: 'reason'"}],
        )
        self.assertGreaterEqual(len(queued), 1)

    def test_benign_capacitor_user_errors_not_queued(self):
        for err_str in (
            "UserError('CustomInfo', {'info': 'NotEnoughCapacitor: Insufficient capacitor to activate module'})",
            "UserError('CustomInfo', {'info': 'NotEnoughCapacitorToOnline: Requires 75% capacitor'})",
            "UserError('CustomInfo', {'info': 'ShipAlreadyInWarp: Cannot initiate or redirect warp while already in warp'})",
            "UserError('CustomInfo', {'info': 'ShipInWarp: Cannot eject while warping'})",
        ):
            with self.subTest(err=err_str):
                with tempfile.TemporaryDirectory() as td:
                    root = Path(td)
                    qdir = root / "queue"
                    sdir = root / "step_1"
                    qdir.mkdir()
                    sdir.mkdir()
                    shot = root / "shot.png"
                    shot.write_bytes(b"png")
                    tb = root / "tb.log"
                    tb.write_text("", encoding="utf-8")
                    srv = root / "srv.log"
                    srv.write_text("", encoding="utf-8")
                    mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
                    oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
                    decision = {
                        "action": {
                            "type": "click_widget",
                            "target": "ModuleButton_434",
                            "expected_ui_and_state_change": "dogmaIM.Activate succeeds",
                        },
                        "bug_report": {"should_file": False},
                    }
                    exec_res = {
                        "action_ok": False,
                        "action_error": f"Client action rejected with {err_str}",
                        "flight_recorder": {
                            "log_exceptions": [],
                            "quasar_errors": [],
                            "macho_exceptions": [
                                {"service": "dogmaIM", "method": "Activate", "error": err_str}
                            ],
                        },
                    }
                    queued = oracle.evaluate_step(
                        step_num=1,
                        git_head="deadbeef",
                        decision=decision,
                        exec_res=exec_res,
                        ui_snapshot={},
                        screenshot_path=shot,
                        burst_paths=[],
                        traceback_log=tb,
                        server_log=srv,
                        step_dir=sdir,
                    )
                    self.assertEqual(queued, [])

    def test_benign_docking_approach_user_error_not_queued(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            sdir = root / "step_1"
            qdir.mkdir()
            sdir.mkdir()
            shot = root / "shot.png"
            shot.write_bytes(b"png")
            tb = root / "tb.log"
            tb.write_text("", encoding="utf-8")
            srv = root / "srv.log"
            srv.write_text("", encoding="utf-8")
            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            decision = {
                "action": {
                    "type": "dock",
                    "target": "60000394",
                    "expected_ui_and_state_change": "Ship docks cleanly at station 60000394",
                },
                "bug_report": {"should_file": False},
            }
            dock_err = {"service": "stationSvc", "method": "Dock", "error": "UserError('DockingApproach', {})"}
            exec_res = {
                "action_ok": True,
                "action_error": None,
                "flight_recorder": {
                    "log_exceptions": [],
                    "quasar_errors": [],
                    "macho_exceptions": [dock_err],
                },
            }
            queued = oracle.evaluate_step(
                step_num=1,
                git_head="deadbeef",
                decision=decision,
                exec_res=exec_res,
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(queued, [])
            self.assertTrue(playtester.PlaytesterBugOracle.is_expected_user_error(dock_err))
            self.assertFalse(
                playtester.PlaytesterBugOracle.is_expected_user_error(
                    {"service": "dogmaIM", "method": "Activate", "error": "RuntimeError('boom')"}
                )
            )
            macho_only_res = {
                "action_ok": False,
                "action_error": "beyonce.CmdStargateJump: GateClosed",
                "flight_recorder": {
                    "log_exceptions": [],
                    "quasar_errors": [],
                    "macho_exceptions": [
                        {"kind": "macho", "service": "beyonce", "method": "CmdStargateJump", "error": "UserError('CustomInfo', {'info': (101, 'UI/GateIcons/GateClosed')})"}
                    ],
                },
            }
            macho_queued = oracle.evaluate_step(
                step_num=2,
                git_head="deadbeef",
                decision={"action": {"type": "jump_gate", "target": "Urlen", "expected_ui_and_state_change": ""}, "bug_report": {"should_file": False}},
                exec_res=macho_only_res,
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(len(macho_queued), 1)
            payload = json.loads(Path(macho_queued[0]).read_text(encoding="utf-8"))
            self.assertEqual(payload["manual_report"]["category_id"], "macho")
            self.assertIn("MachoNet error on jump_gate (beyonce.CmdStargateJump)", payload["manual_report"]["title"])
            self.assertNotIn("Quasar", payload["manual_report"]["title"])



class TestCorruptedViewstateDesyncRecovery(unittest.TestCase):
    def test_detect_corrupted_viewstate_desync_healthy_vs_broken(self):
        self.assertIsNone(playtester._detect_corrupted_viewstate_desync(None))
        self.assertIsNone(
            playtester._detect_corrupted_viewstate_desync(
                {"active_view": "hangar", "space": {"in_space": False, "nearby_balls": [], "ego": None}}
            )
        )
        self.assertIsNone(
            playtester._detect_corrupted_viewstate_desync(
                {
                    "active_view": "inflight",
                    "space": {
                        "in_space": True,
                        "nearby_balls": [{"ball_id": 40000001}],
                        "ego": {"type_id": 32880, "mode": 2},
                    },
                }
            )
        )
        self.assertEqual(
            playtester._detect_corrupted_viewstate_desync(
                {
                    "active_view": "inflight",
                    "space": {
                        "in_space": True,
                        "nearby_balls": [],
                        "ego": {"type_id": 0, "mode": -1},
                    },
                }
            ),
            "in_space_empty_ballpark_uninitialized_ego",
        )
        self.assertEqual(
            playtester._detect_corrupted_viewstate_desync(
                {
                    "active_view": "inflight",
                    "space": {"in_space": False, "nearby_balls": [], "ego": None},
                }
            ),
            "view_inflight_not_in_space",
        )

    def test_maybe_recover_restarts_only_client_after_consecutive_desyncs(self):
        runner = object.__new__(playtester.PlaytesterRunner)
        runner.corrupted_viewstate_streak = 0
        calls = []
        runner.reset_batch_state = lambda: calls.append("reset_batch")
        runner.stop_client = lambda: calls.append("stop_client")
        runner.start_client = lambda: calls.append("start_client") or True

        bad_space = {
            "active_view": "inflight",
            "space": {"in_space": True, "nearby_balls": [], "ego": {"type_id": 0, "mode": -1}},
        }
        bad_dock = {
            "active_view": "inflight",
            "space": {"in_space": False, "nearby_balls": [], "ego": None},
        }
        healthy = {
            "active_view": "hangar",
            "space": {"in_space": False, "nearby_balls": [], "ego": None},
        }

        self.assertIsNone(runner.maybe_recover_corrupted_viewstate(bad_space, threshold=2))
        self.assertEqual(runner.corrupted_viewstate_streak, 1)
        self.assertEqual(calls, [])

        # Healthy intermediate snapshot resets the streak cleanly.
        self.assertIsNone(runner.maybe_recover_corrupted_viewstate(healthy, threshold=2))
        self.assertEqual(runner.corrupted_viewstate_streak, 0)

        # Two consecutive desync snapshots (even transitioning space->dock view) trigger client-only restart.
        self.assertIsNone(runner.maybe_recover_corrupted_viewstate(bad_space, threshold=2))
        reason = runner.maybe_recover_corrupted_viewstate(bad_dock, threshold=2)
        self.assertEqual(reason, "view_inflight_not_in_space")
        self.assertEqual(runner.corrupted_viewstate_streak, 0)
        self.assertEqual(calls, ["reset_batch", "stop_client", "start_client"])



class TestOracleBoundaryAndTracebacks(unittest.TestCase):


    def test_session_boundary_markers_skip_false_ui_desync_and_stale_prev_action(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            sdir = root / "step_9754"
            qdir.mkdir()
            sdir.mkdir()
            shot = root / "shot.png"
            shot.write_bytes(b"png")
            tb = root / "tb.log"
            tb.write_text("", encoding="utf-8")
            srv = root / "srv.log"
            srv.write_text("", encoding="utf-8")

            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            mem.data["recent_actions"].append(
                {
                    "step": 9752,
                    "action_type": "click_widget",
                    "target": "View Rewards",
                    "ok": True,
                }
            )
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            false_desync_decision = {
                "action": {
                    "type": "undock",
                    "target": "Station Undock",
                    "expected_ui_and_state_change": "Ship undocks",
                },
                "previous_action_verification": {
                    "expectation_met": False,
                    "discrepancy_summary": "JobBoardWindow missing after View Rewards click",
                },
                "bug_report": {
                    "should_file": True,
                    "category": "ui_state_desync",
                    "title": "JobBoardWindow disappeared silently",
                    "severity": "medium",
                    "subsystem": "opportunities",
                    "expected_behavior": "Daily Goals stays open",
                    "actual_behavior": "Window gone",
                    "reproduction_steps": ["Click View Rewards"],
                },
            }
            clean_exec = {
                "action_ok": True,
                "flight_recorder": {
                    "log_exceptions": [],
                    "quasar_errors": [],
                    "macho_exceptions": [],
                },
            }

            for marker in (
                {},
                {"client_restarted_unexpectedly": "client_or_rpc_unreachable"},
                {"client_restarted_for_desync": "stale_viewstate"},
                {"server_hot_reloaded_to": "deadbeef"},
            ):
                with self.subTest(marker=marker):
                    self.assertTrue(playtester._is_session_boundary_flight_recorder(marker))
                    prompt = playtester.build_reasoning_prompt(
                        step_num=9754,
                        screenshot_path=shot,
                        burst_paths=[],
                        ui_snapshot={"active_view": "hangar", "ui": {}, "open_windows": [], "modals": []},
                        last_flight_recorder=marker,
                        memory=mem,
                        git_head="deadbeef",
                    )
                    self.assertIn("Fresh client session just started, restarted, or hot-reloaded", prompt)
                    queued = oracle.evaluate_step(
                        step_num=9754,
                        git_head="deadbeef",
                        decision=false_desync_decision,
                        exec_res=clean_exec,
                        ui_snapshot={},
                        screenshot_path=shot,
                        burst_paths=[],
                        traceback_log=tb,
                        server_log=srv,
                        step_dir=sdir,
                        last_flight_recorder=marker,
                    )
                    self.assertEqual(queued, [])

            # Non-boundary flight recorder still reports Channel 5 bugs as expected.
            normal_fr = {"macho_calls": [{"service": "LPSvc", "method": "GetLP"}], "action_error": None}
            self.assertFalse(playtester._is_session_boundary_flight_recorder(normal_fr))
            queued_real = oracle.evaluate_step(
                step_num=9755,
                git_head="deadbeef",
                decision=false_desync_decision,
                exec_res=clean_exec,
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder=normal_fr,
            )
            self.assertEqual(len(queued_real), 1)


    def test_oracle_catches_unseen_client_traceback_when_hook_fr_empty_and_dedups_repeat(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            sdir = root / "step_1"
            qdir.mkdir()
            sdir.mkdir()
            shot = root / "shot.png"
            shot.write_bytes(b"png")
            tb = root / "tb.log"
            tb_block = (
                "log.LogException args=() kwargs={}\n"
                "Traceback (most recent call last):\n"
                '  File "eve/client/script/ui/inflight/bracketsAndTargets/targetInBar.py", line 370, in SetTargetLabel\n'
                "KeyError: ('RecordNotFound', 500001)\n"
            )
            tb.write_text(tb_block, encoding="utf-8")
            srv = root / "srv.log"
            srv.write_text("", encoding="utf-8")
            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            decision = {"action": {"type": "wait", "target": "3"}, "bug_report": {"should_file": False}}
            exec_res = {"action_ok": True, "flight_recorder": {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []}}

            queued1 = oracle.evaluate_step(
                step_num=1,
                git_head="deadbeef",
                decision=decision,
                exec_res=exec_res,
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(len(queued1), 1)
            self.assertEqual(len(oracle.last_unseen_tracebacks), 1)

            # Appending identical repeating tick spam should be suppressed cleanly by _seen_tb_sigs
            with tb.open("a", encoding="utf-8") as f:
                f.write("\n" + tb_block)
            queued2 = oracle.evaluate_step(
                step_num=2,
                git_head="deadbeef",
                decision=decision,
                exec_res=exec_res,
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(queued2, [])

            # When fr_log_excs contains a known/filed exception before a distinct new one,
            # the new exception must still be queued (never masked by fr_log_excs[0]).
            mem.save()
            queued3 = oracle.evaluate_step(
                step_num=3,
                git_head="deadbeef",
                decision=decision,
                exec_res={
                    "action_ok": True,
                    "flight_recorder": {
                        "log_exceptions": [
                            {
                                "headline": "KeyError: ('RecordNotFound', 500001)",
                                "trace": tb_block,
                            },
                            {
                                "headline": "AttributeError: moonID",
                                "trace": 'File "combatLogPanel.py", line 112\nAttributeError: moonID',
                            },
                        ],
                        "quasar_errors": [],
                        "macho_exceptions": [],
                    },
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(len(queued3), 1)
            self.assertIn("AttributeError: moonID", Path(queued3[0]).read_text(encoding="utf-8"))


    def test_oracle_suppresses_symptom_blank_window_when_suppressed_log_exc_present(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            sdir = root / "step_12045"
            qdir.mkdir()
            sdir.mkdir()
            shot = root / "shot.png"
            shot.write_bytes(b"png")
            tb = root / "tb.log"
            tb.write_text("", encoding="utf-8")
            srv = root / "srv.log"
            srv.write_text("", encoding="utf-8")
            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            tb1 = (
                "stdlogutils.EveExceptionsFormatter logger=root:\n"
                "EXCEPTION #7 : ...\n"
                "KeyError: 0\n"
                "/eve/client/script/ui/shared/neocom/journal.py(470) IsMissionActiveWithAgent\n"
            )
            tb2 = (
                "stdlogutils.EveExceptionsFormatter logger=root:\n"
                "EXCEPTION #99 : ...\n"
                "KeyError: 0\n"
                "/eve/client/script/ui/shared/market/marketWindow.py(99) LoadOrders\n"
            )
            self.assertNotEqual(oracle._tb_frame_sig(tb1), oracle._tb_frame_sig(tb2))

            decision = {
                "action": {"type": "click_xy", "target": ""},
                "previous_action_verification": {
                    "expectation_met": False,
                    "discrepancy_summary": "Window stayed dark behind loading spinner",
                },
                "bug_report": {
                    "should_file": True,
                    "category": "blank_or_stuck_window",
                    "title": "[Agency] Blank content behind spinner",
                    "severity": "high",
                    "subsystem": "ui.agency",
                    "expected_behavior": "Content renders",
                    "actual_behavior": "Spinner stuck",
                    "reproduction_steps": ["Click card"],
                },
            }
            exec_res = {"action_ok": True, "flight_recorder": {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []}}
            last_fr = {
                "log_exceptions": [],
                "suppressed_log_exceptions": [{"headline": "KeyError: 0", "sig": "journal.py(470)"}],
                "macho_exceptions": [],
                "quasar_errors": [],
            }
            queued = oracle.evaluate_step(
                step_num=12045,
                git_head="deadbeef",
                decision=decision,
                exec_res=exec_res,
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder=last_fr,
            )
            self.assertEqual(queued, [])



class TestPersonaCleanupAndExecutorWidgets(unittest.TestCase):
    def test_persona_cleanup_banner_and_channel5_suppression(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            state_dir = root / "state"
            queue_dir = root / "queue"
            step_dir = root / "steps" / "step_9973"
            state_dir.mkdir(parents=True)
            queue_dir.mkdir(parents=True)
            step_dir.mkdir(parents=True)
            screen = step_dir / "screen.png"
            screen.write_bytes(b"\x89PNG\r\n\x1a\n")
            tb_log = root / "client_tracebacks.log"
            srv_log = root / "server.log"
            tb_log.write_text("", encoding="utf-8")
            srv_log.write_text("", encoding="utf-8")

            mem = playtester.PlaytesterMemory(state_dir, git_head="deadbeef")
            oracle = playtester.PlaytesterBugOracle(root, queue_dir, mem, sync_github=False)

            last_fr = {
                "ok": True,
                "action": {"type": "click_widget", "widget_path": "PanelEveMenu/open_structure_cosmetics"},
                "state_diff": {"windows_opened": ["PaintTool"], "windows_closed": []},
                "persona_rotation_cleanup": {
                    "closed_windows": ["watchlistpanel", "PaintTool"],
                    "closed_secondary_view": None,
                    "note": "Harness closed utility windows on persona rotation.",
                },
            }
            ui_snap = {"open_windows": [], "clickable_widgets": []}
            prompt = playtester.build_reasoning_prompt(
                9973, screen, [], ui_snap, last_fr, mem, "deadbeef"
            )
            self.assertIn("Harness Persona-Rotation Cleanup Notice", prompt)
            self.assertIn("PaintTool", prompt)

            # 1. Agent bug_report blaming cleaned window disappears -> suppressed
            dec_bug = {
                "previous_action_verification": {
                    "expectation_met": False,
                    "discrepancy_summary": "PaintTool opened then immediately vanished cleanly",
                },
                "bug_report": {
                    "should_file": True,
                    "title": "open_structure_cosmetics opens PaintTool transiently and closes",
                    "category": "ui_state_desync",
                    "severity": "medium",
                    "subsystem": "client-ui",
                    "expected_behavior": "PaintTool stays open",
                    "actual_behavior": "PaintTool is absent from open_windows",
                    "reproduction_steps": ["Click open_structure_cosmetics"],
                },
            }
            exec_res = {"ok": True, "action": {"type": "press_key", "key": "ESCAPE"}}
            self.assertEqual(
                oracle.evaluate_step(
                    9973, "deadbeef", dec_bug, exec_res, ui_snap, screen, [], tb_log, srv_log, step_dir, last_fr
                ),
                [],
            )

            # 2. Failed pav expectation fallback without bug_report -> suppressed
            dec_pav_only = {
                "previous_action_verification": {
                    "expectation_met": False,
                    "discrepancy_summary": "PaintTool window disappeared before screenshot",
                },
                "bug_report": {"should_file": False},
            }
            self.assertEqual(
                oracle.evaluate_step(
                    9973, "deadbeef", dec_pav_only, exec_res, ui_snap, screen, [], tb_log, srv_log, step_dir, last_fr
                ),
                [],
            )

            # 3. Hard error present -> NOT suppressed even with persona_rotation_cleanup
            last_fr_err = dict(last_fr)
            last_fr_err["macho_exceptions"] = ["RuntimeError: PaintTool crash"]
            queued_err = oracle.evaluate_step(
                9974, "deadbeef", dec_bug, exec_res, ui_snap, screen, [], tb_log, srv_log, step_dir, last_fr_err
            )
            self.assertEqual(len(queued_err), 1)

            # 4. Unrelated bug report (different window, non-overlapping windows_opened) -> NOT suppressed
            last_fr_other = {
                "ok": True,
                "action": {"type": "click_widget", "widget_path": "Neocom/market"},
                "state_diff": {"windows_opened": [], "windows_closed": []},
                "persona_rotation_cleanup": {"closed_windows": ["watchlistpanel"], "closed_secondary_view": None},
            }
            dec_other = {
                "previous_action_verification": {
                    "expectation_met": False,
                    "discrepancy_summary": "MarketWnd stayed stuck blank",
                },
                "bug_report": {
                    "should_file": True,
                    "title": "MarketWnd blank on open",
                    "category": "blank_or_stuck_window",
                    "severity": "medium",
                    "subsystem": "market",
                    "expected_behavior": "MarketWnd renders categories",
                    "actual_behavior": "MarketWnd is empty",
                    "reproduction_steps": ["Click Neocom/market"],
                },
            }
            queued_other = oracle.evaluate_step(
                9975, "deadbeef", dec_other, exec_res, ui_snap, screen, [], tb_log, srv_log, step_dir, last_fr_other
            )
            self.assertEqual(len(queued_other), 1)

    def test_executor_prompt_keeps_140_clickable_widgets(self):
        with tempfile.TemporaryDirectory() as tmp:
            state_dir = Path(tmp) / "state"
            state_dir.mkdir(parents=True)
            screen = Path(tmp) / "screen.png"
            screen.write_bytes(b"\x89PNG\r\n\x1a\n")
            mem = playtester.PlaytesterMemory(state_dir, git_head="deadbeef")
            widgets = [{"path": f"Wnd/btn_{i}", "label": f"widget_{i}"} for i in range(145)]
            ui_snap = {
                "ui": {
                    "open_windows": [],
                    "modals": [],
                    "recent_modal_messages": [],
                    "context_menu": [],
                    "clickable_widgets": widgets,
                }
            }
            plan = {
                "goal": "Click widget_139",
                "instructions": "Click Wnd/btn_139",
                "success_criteria": "btn_139 clicked",
                "max_steps": 2,
            }
            prompt = playtester.build_executor_prompt(
                10, 1, 0, plan, screen, [], ui_snap, {}, mem, []
            )
            widgets_section = prompt.split("### Clickable Widgets", 1)[1]
            self.assertIn("Clickable Widgets (145 total, showing top 110)", prompt)
            self.assertIn("widget_95", widgets_section)
            self.assertIn("widget_109", widgets_section)
            self.assertNotIn("widget_110", widgets_section)


    def test_traceback_headline_ignores_exception_end_and_splits_exception_blocks(self):
        import types
        import importlib.util

        eve_tb = (
            "EXCEPTION #3 logged at 10/11/26 19:51:08 : Could not fetch owned ship cosmetic licenses: \n\n"
            "TimeoutException\n\n"
            "Common path prefix = \n"
            "  File \"packages/shipcosmetics/client/licensegateway/quasar.py\", line 117, in _list_owned_licenses\n"
            "    return list_licenses_response.licenses\n\n"
            "EXCEPTION END\n"
            "EXCEPTION #4 logged at 10/11/26 19:51:08 : LoadingContainer: exception in _LoadContentThread\n\n"
            "RuntimeError: Could not fetch owned ship cosmetic licenses: \n\n"
            "Common path prefix = \n"
            "  File \"eve/client/script/ui/shared/fitting/fittingWnd.py\", line 85, in _LoadContentThread\n"
            "    ex = TimeoutException()\n\n"
            "EXCEPTION END\n"
        )
        # 1. Verify evomu_site._log_exc_line extracts 'TimeoutException', not 'EXCEPTION END'
        site_path = Path(__file__).resolve().parent / "evomu_site.py"
        spec = importlib.util.spec_from_file_location("evomu_site_test", site_path)
        site_mod = importlib.util.module_from_spec(spec)
        fake_builtin = types.ModuleType("__builtin__")
        with mock.patch.dict(sys.modules, {"__builtin__": fake_builtin}):
            spec.loader.exec_module(site_mod)
        site_mod._RECENT_EXCEPTIONS.clear()
        site_mod._log_exc_line(eve_tb.split("EXCEPTION #4")[0])
        self.assertEqual(len(site_mod._RECENT_EXCEPTIONS), 1)
        self.assertEqual(site_mod._RECENT_EXCEPTIONS[0]["headline"], "TimeoutException")

        # 2. Verify PlaytesterBugOracle splits EXCEPTION # blocks, ignores stale 'EXCEPTION END' headlines,
        #    and preserves both TimeoutException and RuntimeError cleanly.
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            sdir = root / "step_1"
            qdir.mkdir()
            sdir.mkdir()
            shot = root / "shot.png"
            shot.write_bytes(b"png")
            tb = root / "tb.log"
            tb.write_text(eve_tb, encoding="utf-8")
            srv = root / "srv.log"
            srv.write_text("", encoding="utf-8")
            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            # Even if hook reports stale headline='EXCEPTION END', oracle recovers 'TimeoutException'
            queued = oracle.evaluate_step(
                step_num=1,
                git_head="deadbeef",
                decision={"action": {"type": "wait", "target": "1"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": True,
                    "flight_recorder": {
                        "log_exceptions": [
                            {
                                "headline": "EXCEPTION END",
                                "trace": eve_tb.split("EXCEPTION #4")[0],
                            }
                        ],
                        "quasar_errors": [],
                        "macho_exceptions": [],
                    },
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            titles = [json.loads(Path(p).read_text(encoding="utf-8"))["title"] for p in queued]
            self.assertEqual(len(titles), 2)
            self.assertEqual(titles[0], "[Playtester] Client exception: TimeoutException")
            self.assertEqual(
                titles[1],
                "[Playtester] Client exception: RuntimeError: Could not fetch owned ship cosmetic licenses:",
            )
            self.assertFalse(any("EXCEPTION END" in t for t in titles))


class TestPostCutoverVerificationScheduler(unittest.TestCase):
    def test_rebuild_clears_incident_fingerprints_and_verification_attempts(self):
        with tempfile.TemporaryDirectory() as td:
            wt = Path(td)
            pdir = wt / ".playtester"
            pdir.mkdir(parents=True)
            (pdir / "memory.json").write_text(
                json.dumps(
                    {
                        "total_steps": 100,
                        "last_server_git_head": "aaaa1111",
                        "last_server_bin_mtime": 1000.0,
                        "filed_incident_fingerprints": ["fp1", "tb:abcd"],
                        "verification_attempts": {"770": {"attempts": 2}},
                        "coverage_frontier": [],
                    }
                ),
                encoding="utf-8",
            )
            mem = playtester.PlaytesterMemory(pdir, git_head="aaaa1111", server_bin_mtime=1000.0)
            # Manual curation without rebuild preserves dedup fingerprints
            mem.normalize_coverage_frontier(
                git_head="aaaa1111",
                server_bin_mtime=1000.0,
                decay_stale_buggy=True,
                save=False,
            )
            self.assertEqual(mem.data["filed_incident_fingerprints"], ["fp1", "tb:abcd"])
            self.assertEqual(mem.data["verification_attempts"], {"770": {"attempts": 2}})

            # Rebuilt git_head MUST clear incident fingerprints so fix-failed repros are never suppressed
            stats = mem.normalize_coverage_frontier(
                git_head="bbbb2222",
                server_bin_mtime=2000.0,
                save=False,
            )
            self.assertTrue(stats["server_rebuilt"])
            self.assertEqual(mem.data["filed_incident_fingerprints"], [])
            self.assertEqual(mem.data["verification_attempts"], {})

    def test_verification_queue_clustering_view_filter_and_anti_stall(self):
        with tempfile.TemporaryDirectory() as td:
            pdir = Path(td) / ".playtester"
            pdir.mkdir(parents=True)
            mem = playtester.PlaytesterMemory(pdir, git_head="deadbeef")
            queue_items = [
                {"issue": 788, "cluster": "docked_reprocessing", "view": "docked", "title": "Reprocessing ship ore", "repro_hint": "Quote ore in Mining Hold", "status": "pending"},
                {"issue": 811, "cluster": "docked_reprocessing", "view": "docked", "title": "Reprocess location violation", "repro_hint": "Reprocess ore", "status": "pending"},
                {"issue": 812, "cluster": "docked_reprocessing", "view": "docked", "title": "Reprocess tuple return", "repro_hint": "Click reprocessButton", "status": "pending"},
                {"issue": 770, "cluster": "docked_wallet_market", "view": "docked", "title": "Wallet legacy ref_type", "repro_hint": "Open Wallet Journal", "status": "pending"},
                {"issue": 815, "cluster": "space_mining", "view": "space", "title": "Miner I OnItemChange", "repro_hint": "Cycle Miner I on Veldspar", "status": "pending"},
                {"issue": 821, "cluster": "space_mining", "view": "space", "title": "Mining ore deposit view", "repro_hint": "Check Mining Hold after cycle", "status": "pending"},
                {"issue": 806, "cluster": "space_undock_nav", "view": "space", "title": "Blocked by #942", "repro_hint": "Ice Belts", "status": "pending", "blocked_by": 942},
            ]
            (pdir / "pending_verifications.json").write_text(json.dumps(queue_items), encoding="utf-8")

            loaded_items = playtester.load_pending_verifications(pdir)
            self.assertTrue(all(x["issue"] != 806 for x in loaded_items))

            # Docked view prioritizes docked_reprocessing cluster (largest fresh view-compatible group)
            pp = playtester.build_planner_prompt(
                10, Path(td) / "screen.png", [], {"active_view": "hangar"}, {}, mem, "deadbeef"
            )
            self.assertIn("### Post-Cutover Fix Verification Queue", pp)
            self.assertIn("matching view=docked", pp)
            self.assertIn("cluster=docked_reprocessing", pp)
            self.assertIn('"issue": 788', pp)
            self.assertNotIn('"issue": 815', pp)

            # In space view, space_mining is chosen instead
            pp_space = playtester.build_planner_prompt(
                11, Path(td) / "screen.png", [], {"active_view": "inflight"}, {}, mem, "deadbeef"
            )
            self.assertIn("matching view=space", pp_space)
            self.assertIn("cluster=space_mining", pp_space)
            self.assertIn('"issue": 815', pp_space)

            # Anti-stall: 2 failed/stalled attempts push #788/#811/#812 behind fresh docked_wallet_market (#770)
            for step in (12, 13):
                mem.apply_planner_decision(
                    step,
                    {
                        "current_goal": "Verify #788 and #811 and #812 in Reprocessing Plant",
                        "delegated_plan": {"goal": "Reprocess Veldspar (#788, #811, #812)"},
                        "memory_update": {"long_term_note_to_append": "Retrying #788 #811 #812"},
                    },
                )
            pp_after_stall = playtester.build_planner_prompt(
                14, Path(td) / "screen.png", [], {"active_view": "hangar"}, {}, mem, "deadbeef"
            )
            self.assertIn("cluster=docked_wallet_market", pp_after_stall)

            # VERIFIED_FIXED supports multi-issue lists ('#770 & #780') and ignores Batch #35 noise
            mem.apply_planner_decision(
                15,
                {
                    "current_goal": "Batch #35: Check Wallet Journal for #770 and #780",
                    "memory_update": {"long_term_note_to_append": "Batch #35 VERIFIED_FIXED #770 & #780 Wallet Journal rendered cleanly"},
                },
            )
            self.assertNotIn("35", mem.data.get("verification_attempts") or {})
            self.assertTrue((mem.data.get("verification_attempts") or {}).get("770", {}).get("verified_fixed"))
            self.assertTrue((mem.data.get("verification_attempts") or {}).get("780", {}).get("verified_fixed"))
            selected, stats = playtester.select_verification_focus(
                playtester.load_pending_verifications(pdir),
                active_view="docked",
                verification_attempts=mem.data.get("verification_attempts"),
            )
            self.assertTrue(all(x["issue"] != 770 for x in selected))
            self.assertEqual(stats["skipped_resolved"], 1)



class TestSlashCommandListSurfaceHost(unittest.TestCase):


    def test_apply_agent_decision_preserves_full_slash_result_for_planner(self):
        with tempfile.TemporaryDirectory() as td:
            pdir = Path(td)
            mem = playtester.PlaytesterMemory(pdir, git_head="deadbeef")
            long_cmds = "Commands: [" + ", ".join(f"'cmd_{i:02d}'" for i in range(60)) + "]"
            self.assertGreater(len(long_cmds), 400)
            mem.apply_agent_decision(
                10,
                {"action": {"type": "slash_command", "command": "/"}},
                {"action_ok": True, "slash_result": long_cmds, "state_diff": {"ui_changed": True}},
            )
            recent = mem.data["recent_actions"][-1]
            self.assertEqual(recent.get("slash_result"), long_cmds)


class TestAgyTranscriptFinishRecovery(unittest.TestCase):
    def test_invoke_agy_executor_prefers_latest_turn_finish_over_stale_stdout(self):
        with tempfile.TemporaryDirectory() as td:
            home_dir = Path(td)
            conv_id = "134f8149-test-conv"
            logs_dir = (
                home_dir
                / ".gemini"
                / "antigravity-cli"
                / "brain"
                / conv_id
                / ".system_generated"
                / "logs"
            )
            logs_dir.mkdir(parents=True, exist_ok=True)
            transcript_rows = [
                {
                    "type": "USER_INPUT",
                    "content": "### Batch #106 Continuation — Global Step 11437 (Batch Step 2 of 7)",
                },
                {
                    "type": "PLANNER_RESPONSE",
                    "thinking": "Dismiss menu first.",
                    "tool_calls": [
                        {
                            "name": "finish",
                            "args": {
                                "batch_step_number": 2,
                                "batch_status": "continue",
                                "escalation_reason": "",
                                "observation": "stale step 2",
                                "previous_action_verification": {"expectation_met": True, "discrepancy_summary": ""},
                                "reasoning": "stale",
                                "action": {"type": "press_key", "target": "ESCAPE", "command": "ESCAPE", "x": None, "y": None, "dest_x": None, "dest_y": None, "right_click": False, "double_click": False, "wait_ms": 500, "expected_ui_and_state_change": "close"},
                            },
                        }
                    ],
                },
                {
                    "type": "USER_INPUT",
                    "content": "### Batch #106 Continuation — Global Step 11438 (Batch Step 3 of 7)",
                },
                {
                    "type": "PLANNER_RESPONSE",
                    "thinking": "Now double click ItemEntry_17938.",
                    "tool_calls": [
                        {
                            "name": "finish",
                            "args": {
                                "batch_step_number": 3,
                                "batch_status": "continue",
                                "escalation_reason": "",
                                "observation": "Menu closed cleanly.",
                                "previous_action_verification": {"expectation_met": True, "discrepancy_summary": ""},
                                "reasoning": "Double click ItemEntry_17938",
                                "action": {"type": "click_widget", "target": "ItemEntry_17938", "command": None, "x": 970, "y": 698, "dest_x": None, "dest_y": None, "right_click": False, "double_click": True, "wait_ms": 500, "expected_ui_and_state_change": "InfoWindow opens"},
                            },
                        }
                    ],
                },
            ]
            (logs_dir / "transcript_full.jsonl").write_text(
                "\n".join(json.dumps(r) for r in transcript_rows) + "\n",
                encoding="utf-8",
            )
            stale_stdout = json.dumps(
                {
                    "conversation_id": conv_id,
                    "status": "SUCCESS",
                    "num_turns": 3,
                    "structured_output": transcript_rows[1]["tool_calls"][0]["args"],
                }
            )
            step_dir = Path(td) / "step_11438"
            fake_proc = types.SimpleNamespace(returncode=0, stdout=stale_stdout, stderr="")
            with mock.patch("os.path.expanduser", side_effect=lambda p: p.replace("~", str(home_dir))), \
                 mock.patch.object(playtester.subprocess, "run", return_value=fake_proc):
                dec, used_model, out_conv, _ = playtester.invoke_agy_executor(
                    "### Batch #106 Continuation — Global Step 11438 (Batch Step 3 of 7)",
                    step_dir=step_dir,
                    model_chain=["hydrogen-l-tf-medium"],
                    conversation_id=conv_id,
                    batch_id=106,
                    batch_step_idx=3,
                    max_batch_steps=7,
                )
                self.assertEqual(used_model, "hydrogen-l-tf-medium")
                self.assertEqual(out_conv, conv_id)
                self.assertEqual(dec.get("batch_step_number"), 3)
                self.assertEqual(dec["action"]["type"], "click_widget")
                self.assertEqual(dec["action"]["target"], "ItemEntry_17938")
                self.assertTrue(dec["action"]["double_click"])

                # Guard returns None if the transcript's latest USER_INPUT belongs to another step
                self.assertIsNone(
                    playtester._extract_agy_latest_finish_decision(
                        conv_id, required_key="action", expected_marker="Global Step 99999"
                    )
                )


class TestStaleControlFileCleanup(unittest.TestCase):
    def test_run_clears_stale_stop_control_file_before_step_loop(self):
        with tempfile.TemporaryDirectory() as td:
            ctrl = Path(td) / "evomu_playtester.control"
            pidf = Path(td) / "evomu_playtester.pid"
            ctrl.write_text("STOP stale\n", encoding="utf-8")

            runner = playtester.PlaytesterRunner.__new__(playtester.PlaytesterRunner)
            runner.args = types.SimpleNamespace(
                reset_db=False,
                max_steps=2,
                continuous=True,
                watch_origin_interval=0,
                hybrid=True,
            )
            runner.hybrid_mode = True
            runner.planner_model_chain = ["gemini-flash-next"]
            runner.executor_model_chain = ["hydrogen-l-tf-medium"]
            runner.worktree_dir = Path(td)
            runner.current_run_dir = Path(td) / "run"
            runner.current_run_dir.mkdir()
            runner.memory = types.SimpleNamespace(data={"total_steps": 0})
            runner.oracle = types.SimpleNamespace(reset_log_offsets=lambda *a: None)
            runner.server_proc = types.SimpleNamespace(poll=lambda: None)
            runner.client_proc = types.SimpleNamespace(poll=lambda: None)
            runner.rpc = types.SimpleNamespace(ping=lambda: True)
            runner.reload_requested = False
            runner.stop_requested = False
            runner.start_server = lambda reset_db=False: True
            runner.start_client = lambda: True
            runner.stop_client = lambda: None
            runner.stop_server = lambda: None
            runner.maybe_recover_corrupted_viewstate = lambda snap: None
            runner.agy_pool = None

            executed = []
            runner.execute_step = lambda step_num, last_fr, last_burst: (
                executed.append(step_num) or ({}, [])
            )

            with mock.patch.object(playtester, "DEFAULT_CONTROL_FILE", ctrl), \
                 mock.patch.object(playtester, "DEFAULT_PID_FILE", pidf), \
                 mock.patch.object(playtester, "AgyStreamWorkerPool") as m_pool_cls:
                rc = runner.run()

            m_pool_cls.assert_called_once()
            m_pool_cls.return_value.prewarm_all.assert_called_once()
            m_pool_cls.return_value.shutdown.assert_called_once()
            self.assertEqual(rc, 0)
            self.assertEqual(executed, [1, 2])
            self.assertFalse(runner.stop_requested)
            self.assertFalse(ctrl.exists())



class TestSdeZeroByteFallback(unittest.TestCase):
    def test_resolve_sde_and_quasar_paths_skips_zero_byte_files(self):
        import os
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            wt = root / "EvoMu-playtester"
            (wt / "data").mkdir(parents=True, exist_ok=True)
            (wt / "data" / "evomu_sde.sqlite").write_bytes(b"")
            (wt / "data" / "quasar_descriptors.sqlite").write_bytes(b"")
            fallback_dir = root / "EvoMu" / "data"
            fallback_dir.mkdir(parents=True, exist_ok=True)
            fallback_sde = fallback_dir / "evomu_sde.sqlite"
            fallback_sde.write_bytes(b"sqlite-data")
            fallback_qd = fallback_dir / "quasar_descriptors.sqlite"
            fallback_qd.write_bytes(b"qd-data")
            driver = playtester.PlaytesterRunner.__new__(playtester.PlaytesterRunner)
            driver.worktree_dir = wt
            with mock.patch.dict(
                os.environ,
                {"EVOMU_SDE_PATH": "", "EVOMU_QUASAR_DESCRIPTORS_PATH": ""},
            ), mock.patch.object(playtester, "REPO_ROOT", wt):
                sde_p, qd_p = driver._resolve_sde_and_quasar_paths()
            self.assertEqual(sde_p, fallback_sde)
            self.assertEqual(qd_p, fallback_qd)



class TestBugOracleCrossStepIsolation(unittest.TestCase):
    def test_channel5_never_attaches_step_n_traceback_and_suppresses_channel2_repeat(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            sdir = root / "step_12799"
            qdir.mkdir()
            sdir.mkdir()
            shot = root / "shot.png"
            shot.write_bytes(b"png")
            tb = root / "tb.log"
            tb.write_text("", encoding="utf-8")
            srv = root / "srv.log"
            srv.write_text("", encoding="utf-8")
            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            # 1. Channel 5 reporting step N-1 desync must not grab step N's traceback from tb.log
            tb.write_text(
                "EXCEPTION #22 logged at 12/11/26 05:41:04 : Agents failed\n"
                "KeyError: 0\n"
                "/eve/client/script/ui/shared/dockedUI/agents.py(122) _load\n"
                "EXCEPTION END\n",
                encoding="utf-8",
            )
            dec_desync = {
                "action": {"type": "click_widget", "target": "stationInformationTabAgents"},
                "previous_action_verification": {
                    "expectation_met": False,
                    "discrepancy_summary": "Previous window toggle closed without updating list",
                },
                "bug_report": {
                    "should_file": True,
                    "category": "ui_state_desync",
                    "title": "[Window] Toggle did not update list",
                    "severity": "medium",
                    "subsystem": "client-ui",
                    "expected_behavior": "List updates",
                    "actual_behavior": "List stale",
                    "reproduction_steps": ["Click toggle"],
                },
            }
            queued_n = oracle.evaluate_step(
                step_num=12799,
                git_head="deadbeef",
                decision=dec_desync,
                exec_res={"action_ok": True, "flight_recorder": {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []}},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            # One bundle from Channel 1 (dockedUI/agents.py) and one from Channel 5 (ui_state_desync)
            ch5_bundles = [p for p in queued_n if "ui_state_desync" in json.loads(p.read_text())["kind"]]
            self.assertEqual(len(ch5_bundles), 1)
            self.assertEqual(json.loads(ch5_bundles[0].read_text())["trace"], "")

            # 2. Channel 5 suppresses redundant broken_game_mechanic report when Channel 2 already captured step N-1 UserError
            for p in qdir.glob("*.json"):
                p.unlink()
            dec_repeat_rpc = {
                "action": {"type": "click_widget", "target": "otherTab"},
                "previous_action_verification": {
                    "expectation_met": False,
                    "discrepancy_summary": "InstallJob failed with InvalidIndustryJobParameters",
                },
                "bug_report": {
                    "should_file": True,
                    "category": "broken_game_mechanic",
                    "title": "[industryManager / InstallJob] Manufacturing rejected",
                    "severity": "high",
                    "subsystem": "industryManager / InstallJob",
                    "expected_behavior": "Starts job",
                    "actual_behavior": "Rejected with UserError",
                    "reproduction_steps": ["Click Start"],
                },
            }
            queued_repeat = oracle.evaluate_step(
                step_num=12800,
                git_head="deadbeef",
                decision=dec_repeat_rpc,
                exec_res={"action_ok": True, "flight_recorder": {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []}},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={
                    "log_exceptions": [],
                    "quasar_errors": [],
                    "action_error": "Client action rejected with UserError(CustomInfo: InvalidIndustryJobParameters)",
                    "expected_ui_and_state_change": "Manufacturing job starts cleanly",
                    "macho_exceptions": [
                        {
                            "service": "industryManager",
                            "method": "InstallJob",
                            "error": "UserError('CustomInfo', {'info': 'InvalidIndustryJobParameters: licensedRuns'})",
                        }
                    ],
                },
            )
            self.assertEqual(queued_repeat, [])



class TestDefaultModelChainHydrogen(unittest.TestCase):
    def test_default_model_chain_uses_hydrogen_high_first_with_fallbacks(self):
        self.assertEqual(playtester.DEFAULT_MODEL_CHAIN[0], "hydrogen-l-tf-high")
        self.assertGreaterEqual(len(playtester.DEFAULT_MODEL_CHAIN), 3)
        self.assertIn("gemini-flash-next", playtester.DEFAULT_MODEL_CHAIN[1:])
        self.assertEqual(
            playtester._resolve_agy_model_args("hydrogen-l-tf-high"),
            ["--model", "hydrogen-l-tf-high"],
        )
        self.assertEqual(
            playtester._resolve_agy_model_args("hydrogen-high"),
            ["--model", "hydrogen-l-tf-high"],
        )


class TestFullSizeWindowAndScreenshotDownscale(unittest.TestCase):
    def test_resolve_playtester_window_spec_full_and_explicit(self):
        retina = {
            "frameWidth": 1728,
            "frameHeight": 1117,
            "visibleMinX": 0,
            "visibleMinY": 0,
            "visibleWidth": 1728,
            "visibleHeight": 1084,
            "backingScaleFactor": 2.0,
        }
        full_spec = playtester.resolve_playtester_window_spec("full", display_info=retina)
        self.assertEqual(full_spec["width"], 3456)
        self.assertEqual(full_spec["height"], (1084 - 28) * 2)
        self.assertEqual(full_spec["left"], 0)
        self.assertEqual(full_spec["top"], 0)
        self.assertEqual(full_spec["ui_scale"], 2.0)

        compact_spec = playtester.resolve_playtester_window_spec("1280x800", display_info=retina)
        self.assertEqual(compact_spec, {"width": 1280, "height": 800, "left": 40, "top": 40, "ui_scale": 1.0})

    def test_downscale_checks_png_dimensions_even_under_250kb(self):
        import struct
        with tempfile.TemporaryDirectory() as td:
            p_large = Path(td) / "dark_space_3456x2112.png"
            ihdr_large = b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13) + b"IHDR" + struct.pack(">II", 3456, 2112) + b"\x08\x06\x00\x00\x00" + b"\x00\x00\x00\x00"
            p_large.write_bytes(ihdr_large)
            self.assertEqual(playtester._read_image_dimensions(p_large), (3456, 2112))

            p_small = Path(td) / "already_1024x626.png"
            ihdr_small = b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13) + b"IHDR" + struct.pack(">II", 1024, 626) + b"\x08\x06\x00\x00\x00" + b"\x00\x00\x00\x00"
            p_small.write_bytes(ihdr_small)
            self.assertEqual(playtester._read_image_dimensions(p_small), (1024, 626))

            with mock.patch.object(playtester.subprocess, "run") as m_run:
                playtester._downscale_screenshot_if_needed(p_large, max_dim=1024)
                m_run.assert_called_once()
                self.assertEqual(m_run.call_args[0][0], ["sips", "-Z", "1024", str(p_large)])

                m_run.reset_mock()
                playtester._downscale_screenshot_if_needed(p_small, max_dim=1024)
                m_run.assert_not_called()


class TestSpeedupOptimizations(unittest.TestCase):
    def test_no_38_flash_and_hydrogen_low_aliases(self):
        for chain in (
            playtester.DEFAULT_MODEL_CHAIN,
            playtester.DEFAULT_EXECUTOR_MODEL_CHAIN,
            playtester.DEFAULT_ROUTINE_PLANNER_MODEL_CHAIN,
        ):
            self.assertGreaterEqual(len(chain), 2)
            for m in chain:
                self.assertNotIn("3.8-flash", m)
        self.assertEqual(playtester.DEFAULT_MODEL_CHAIN[0], "hydrogen-l-tf-high")
        self.assertEqual(playtester.DEFAULT_EXECUTOR_MODEL_CHAIN[0], "hydrogen-l-tf-medium")
        self.assertEqual(playtester.DEFAULT_ROUTINE_PLANNER_MODEL_CHAIN[0], "hydrogen-l-tf-medium")
        self.assertEqual(
            playtester._resolve_agy_model_args("gemini-hydrogen-low"),
            ["--model", "hydrogen-l-tf-medium"],
        )
        self.assertEqual(
            playtester._resolve_agy_model_args("hydrogen-l-tf-low"),
            ["--model", "hydrogen-l-tf-medium"],
        )


    def test_window_info_cache_and_compact_widgets(self):
        playtester._WINDOW_INFO_CACHE.clear()
        calls = []

        def fake_subproc(cmd, capture_output=True, text=True, timeout=5):
            calls.append(cmd)
            return types.SimpleNamespace(
                returncode=0,
                stdout=json.dumps({"id": 777, "x": 0, "y": 0, "width": 3456, "height": 2112, "onscreen": True}),
            )

        fake_bin = mock.Mock()
        fake_bin.exists.return_value = True
        with mock.patch.object(playtester, "_ensure_window_info_helper", return_value=fake_bin), \
             mock.patch.object(playtester.subprocess, "run", side_effect=fake_subproc):
            w1 = playtester.find_exefile_window(1234)
            w2 = playtester.find_exefile_window(1234)
            w3 = playtester.find_exefile_window(1234, force_refresh=True)
        self.assertEqual(w1["id"], 777)
        self.assertEqual(w2["id"], 777)
        self.assertEqual(w3["id"], 777)
        self.assertEqual(len(calls), 2)

        compact = playtester._format_compact_widgets(
            [{"name": "btn", "path": "Wnd/btn", "label": "Click", "x": 10, "y": 20, "editable": True}]
        )
        self.assertIn('btn | Wnd/btn label="Click" @(10,20) [editable]', compact)

    def test_resolve_direct_action_and_step_assertions(self):
        ui_snap = {
            "session": {"stationid": None, "solarsystemid2": 30000142},
            "ui": {
                "open_windows": [{"window_id": "MarketWindow"}],
                "modals": [],
                "context_menu": [{"name": "context_menu_WarpToWithin", "label": "Warp to Location", "x": 500, "y": 400}],
                "clickable_widgets": [
                    {"name": "quickFilterInputBox", "path": "MarketWindow/quickFilterInputBox", "label": "Filter", "x": 200, "y": 150, "editable": True}
                ],
            },
            "space": {"nearby_balls": [{"item_id": 40009080, "type_id": 6, "name": "Stargate"}]},
        }
        act_ctx, err_ctx = playtester._resolve_direct_action(
            {"action": {"type": "click_widget", "target": "Warp to Location"}},
            ui_snap,
        )
        self.assertEqual(err_ctx, "")
        self.assertEqual(act_ctx["target"], "context_menu_WarpToWithin")
        self.assertEqual((act_ctx["x"], act_ctx["y"]), (500, 400))

        act_txt, err_txt = playtester._resolve_direct_action(
            {"action": {"type": "type_text", "target": "quickFilterInputBox", "command": "Tritanium\n"}},
            ui_snap,
        )
        self.assertEqual(err_txt, "")
        self.assertEqual(act_txt["x"], 200)

        act_miss, err_miss = playtester._resolve_direct_action(
            {"action": {"type": "click_widget", "target": "nonexistentButton"}},
            ui_snap,
        )
        self.assertIsNone(act_miss)
        self.assertIn("not found", err_miss)

        drag_snap = {
            "ui": {
                "context_menu": [
                    {"name": "context_menu_clathrates", "label": "Strontium Clathrates", "x": 10, "y": 10}
                ],
                "clickable_widgets": [
                    {
                        "name": "ItemEntry_16275",
                        "path": "InventoryStation/entry_0/ItemEntry_16275",
                        "label": "Strontium Clathrates",
                        "x": 300,
                        "y": 651,
                    },
                    {
                        "name": "ShipHangar",
                        "path": "InventoryStation/tree/ShipHangar",
                        "label": "Ship hangar",
                        "x": 172,
                        "y": 365,
                    },
                ],
            }
        }
        act_drag_missing, err_drag_missing = playtester._resolve_direct_action(
            {"action": {"type": "drag_widget", "target": "ItemEntry_16275"}},
            drag_snap,
        )
        self.assertIsNone(act_drag_missing)
        self.assertIn("dest_x/dest_y", err_drag_missing)

        act_drag_zero, err_drag_zero = playtester._resolve_direct_action(
            {"action": {"type": "drag_widget", "target": "ItemEntry_16275", "dest_x": 0, "dest_y": 0}},
            drag_snap,
        )
        self.assertIsNone(act_drag_zero)
        self.assertIn("positive dest_x/dest_y", err_drag_zero)

        act_drag_dest_target, err_drag_dest_target = playtester._resolve_direct_action(
            {"action": {"type": "drag_widget", "target": "Strontium Clathrates", "dest_target": "ShipHangar"}},
            drag_snap,
        )
        self.assertEqual(err_drag_dest_target, "")
        self.assertEqual(act_drag_dest_target["target"], "InventoryStation/entry_0/ItemEntry_16275")
        self.assertEqual((act_drag_dest_target["x"], act_drag_dest_target["y"]), (300, 651))
        self.assertEqual((act_drag_dest_target["dest_x"], act_drag_dest_target["dest_y"]), (172, 365))

        act_drag_bad_dest, err_drag_bad_dest = playtester._resolve_direct_action(
            {"action": {"type": "drag_widget", "target": "ItemEntry_16275", "dest_target": "NoSuchHangar"}},
            drag_snap,
        )
        self.assertIsNone(act_drag_bad_dest)
        self.assertIn("destination widget", err_drag_bad_dest)

        act_drag_ok, err_drag_ok = playtester._resolve_direct_action(
            {"action": {"type": "drag_widget", "target": "ItemEntry_16275", "dest_x": 172, "dest_y": 365}},
            drag_snap,
        )
        self.assertEqual(err_drag_ok, "")
        self.assertEqual((act_drag_ok["dest_x"], act_drag_ok["dest_y"]), (172, 365))

        for bad_coords in (
            {"dest_x": -50, "dest_y": 365},
            {"dest_x": 172, "dest_y": -10},
        ):
            act_neg, err_neg = playtester._resolve_direct_action(
                {"action": {"type": "drag_widget", "target": "ItemEntry_16275", **bad_coords}},
                drag_snap,
            )
            self.assertIsNone(act_neg)
            self.assertIn("positive dest_x/dest_y", err_neg)

        for bool_coords in (
            {"dest_x": True, "dest_y": 365},
            {"dest_x": 172, "dest_y": False},
        ):
            act_bool, err_bool = playtester._resolve_direct_action(
                {"action": {"type": "drag_widget", "target": "ItemEntry_16275", **bool_coords}},
                drag_snap,
            )
            self.assertIsNone(act_bool)
            self.assertIn("invalid dest_x/dest_y", err_bool)

        act_zero_with_target, err_zero_with_target = playtester._resolve_direct_action(
            {
                "action": {
                    "type": "drag_widget",
                    "target": "ItemEntry_16275",
                    "dest_x": 0,
                    "dest_y": 0,
                    "dest_target": "ShipHangar",
                }
            },
            drag_snap,
        )
        self.assertEqual(err_zero_with_target, "")
        self.assertEqual((act_zero_with_target["dest_x"], act_zero_with_target["dest_y"]), (172, 365))

        act_float_coords, err_float_coords = playtester._resolve_direct_action(
            {"action": {"type": "drag_widget", "target": "ItemEntry_16275", "dest_x": 172.0, "dest_y": "365"}},
            drag_snap,
        )
        self.assertEqual(err_float_coords, "")
        self.assertIs(type(act_float_coords["dest_x"]), int)
        self.assertIs(type(act_float_coords["dest_y"]), int)
        self.assertEqual((act_float_coords["dest_x"], act_float_coords["dest_y"]), (172, 365))

        planner_schema = json.loads((HARNESS_DIR / "playtester_planner_schema.json").read_text(encoding="utf-8"))
        plan_act_props = (
            planner_schema["properties"]["delegated_plan"]["properties"]["steps"]["items"]["properties"]["action"]["properties"]
        )
        self.assertEqual(plan_act_props["dest_target"], {"type": ["string", "null"]})
        self.assertEqual(plan_act_props["dest_widget"], {"type": ["string", "null"]})

        for dest_field in ("dest_target", "dest_widget"):
            raw_plan_dec = {
                "observation": "Inventory open",
                "previous_action_verification": {"expectation_met": True, "discrepancy_summary": ""},
                "reasoning": "Drag clathrates",
                "current_goal": "Move cargo",
                "delegated_plan": {
                    "plan_goal": "Move cargo",
                    "horizon_rationale": "Single direct drag",
                    "max_batch_steps": 1,
                    "steps": [
                        {
                            "step_number": 1,
                            "exec_mode": "direct",
                            "action": {
                                "type": "drag_widget",
                                "target": "Strontium Clathrates",
                                dest_field: "ShipHangar",
                            },
                        }
                    ],
                    "success_criteria": "Item moved",
                    "abort_conditions": "Error modal",
                },
                "memory_update": {},
                "bug_report": {"should_file": False},
            }
            norm_plan_dec = playtester._normalize_planner_decision(raw_plan_dec)
            norm_step = norm_plan_dec["delegated_plan"]["steps"][0]
            self.assertEqual(norm_step["action"].get(dest_field), "ShipHangar")
            act_roundtrip, err_roundtrip = playtester._resolve_direct_action(norm_step, drag_snap)
            self.assertEqual(err_roundtrip, "")
            self.assertEqual((act_roundtrip["dest_x"], act_roundtrip["dest_y"]), (172, 365))

        with tempfile.TemporaryDirectory() as td:
            exec_mem = playtester.PlaytesterMemory(Path(td), git_head="deadbeef")
            cont_prompt = playtester.build_executor_prompt(
                step_num=2,
                batch_id=1,
                batch_step_idx=2,
                delegated_plan=raw_plan_dec["delegated_plan"],
                screenshot_path=None,
                burst_paths=[],
                ui_snapshot=drag_snap,
                last_flight_recorder={},
                memory=exec_mem,
                batch_history=[{"step": 1, "ok": True}],
                use_compact_json_contract=True,
            )
            self.assertIn("drag_widget", cont_prompt)
            self.assertIn("dest_x", cont_prompt)
            self.assertIn("dest_y", cont_prompt)

        ok1, msg1 = playtester._evaluate_step_assertions(
            {
                "expect_window_open": "MarketWindow",
                "expect_window_closed": "FittingWindow",
                "expect_modal_absent": True,
                "expect_ball_present": "Stargate",
                "expect_session_field": "solarsystemid2 == 30000142",
            },
            {"state_diff": {}},
            ui_snap,
        )
        self.assertTrue(ok1, msg1)

        ok2, msg2 = playtester._evaluate_step_assertions(
            {"expect_window_open": "InventoryStation"},
            {"state_diff": {}},
            ui_snap,
        )
        self.assertFalse(ok2)
        self.assertIn("expect_window_open", msg2)

        in_space_snap = {
            "session": {"solarsystemid2": 30000142, "stationid": None},
            "space": {"in_space": True},
            "ui": {"open_windows": [], "modals": []},
        }
        ok3, msg3 = playtester._evaluate_step_assertions(
            {"expect_session_field": "{'in_space': True}"},
            {"state_diff": {}},
            in_space_snap,
        )
        self.assertTrue(ok3, msg3)

        ok4, msg4 = playtester._evaluate_step_assertions(
            {"expect_session_field": "in_space == False"},
            {"state_diff": {}},
            in_space_snap,
        )
        self.assertFalse(ok4)
        self.assertIn("expect_session_field", msg4)

    def test_agy_stream_worker_pool_turn_and_429_invalidation(self):
        import os

        class FakePipe:
            def __init__(self):
                self.written = bytearray()
                self.closed = False

            def write(self, b):
                self.written.extend(b)

            def flush(self):
                pass

            def read(self):
                return b""

            def close(self):
                self.closed = True

        r_fd, w_fd = os.pipe()
        os.write(w_fd, (json.dumps({"event": "init", "conversation_id": "conv-1"}) + "\n").encode("utf-8"))
        stdin_pipe = FakePipe()
        stderr_pipe = FakePipe()

        class FakeProc:
            def __init__(self):
                self.stdin = stdin_pipe
                self.stdout = os.fdopen(r_fd, "rb", buffering=0)
                self.stderr = stderr_pipe
                self._rc = None

            def poll(self):
                return self._rc

            def wait(self, timeout=None):
                self._rc = 0
                return 0

            def terminate(self):
                self._rc = -15

            def kill(self):
                self._rc = -9

        fake_proc = FakeProc()
        orig_write = stdin_pipe.write

        def write_and_emit(b):
            orig_write(b)
            os.write(
                w_fd,
                (
                    json.dumps(
                        {
                            "event": "result",
                            "result": {"conversation_id": "conv-1", "status": "SUCCESS", "structured_output": {"ok": True}},
                        }
                    )
                    + "\n"
                ).encode("utf-8"),
            )
            os.close(w_fd)

        stdin_pipe.write = write_and_emit
        with tempfile.TemporaryDirectory() as td:
            pool = playtester.AgyStreamWorkerPool(sandbox_dir=Path(td), standby_specs=[], auto_respawn=False)
            try:
                with mock.patch.object(playtester.subprocess, "Popen", return_value=fake_proc):
                    rc, out, _err, _el = pool.run_turn(Path(td) / "schema.json", "hydrogen-l-tf-medium", "hello world", timeout_secs=10.0)
                self.assertEqual(rc, 0)
                self.assertEqual(json.loads(out)["conversation_id"], "conv-1")
                self.assertTrue(stdin_pipe.closed)
            finally:
                pool.shutdown()

    def test_hybrid_direct_fast_path_and_async_oracle_flush(self):
        with tempfile.TemporaryDirectory() as td:
            td_p = Path(td)
            runner = playtester.PlaytesterRunner.__new__(playtester.PlaytesterRunner)
            runner.args = types.SimpleNamespace(
                hybrid=True,
                foreground=False,
                agy_timeout=30,
                no_github_sync=True,
                os_input_fallback=False,
                continuous=False,
            )
            runner.hybrid_mode = True
            runner.worktree_dir = td_p
            runner.current_run_dir = td_p / "run"
            runner.current_run_dir.mkdir()
            runner.planner_model_chain = ["hydrogen-l-tf-high"]
            runner.routine_planner_model_chain = ["hydrogen-l-tf-medium"]
            runner.executor_model_chain = ["hydrogen-l-tf-medium"]
            runner.screenshot_max_dim = 1024
            runner.server_proc = None
            runner.client_proc = None
            runner.batch_id = 0
            runner.cached_git_head = "abc1234"
            runner.cached_post_state = None
            runner.cached_post_state_ts = 0.0
            runner.agy_pool = None
            runner.last_ui_snapshot = None
            runner.corrupted_viewstate_streak = 0
            runner._escalation_context = None
            runner._escalate_to_diagnostic_planner = False
            runner._force_next_screenshot = False
            runner.reset_batch_state()
            runner.memory = playtester.PlaytesterMemory(td_p / "state", git_head="abc1234")
            runner.pending_persona_cleanup = False
            runner._pending_assertion_escalation = None
            runner.last_active_persona_id = str((runner.memory.data.get("active_persona") or {}).get("id") or "")
            runner.memory.apply_planner_decision = mock.Mock()
            runner.memory.apply_agent_decision = mock.Mock()
            runner.oracle = playtester.PlaytesterBugOracle(
                td_p,
                td_p / ".evomu-bugs" / "queue",
                runner.memory,
                sync_github=False,
                async_write=True,
            )

            post_snap_1 = {
                "active_view": "hangar",
                "session": {"solarsystemid2": 30000142},
                "ui": {"open_windows": [{"window_id": "InventoryStation"}], "modals": [], "clickable_widgets": []},
            }
            rpc_calls = []

            def fake_rpc_call(method, params=None, timeout=15.0):
                rpc_calls.append(method)
                if method == "get_state":
                    return dict(post_snap_1)
                if method == "execute_action":
                    return {
                        "ok": True,
                        "action": params,
                        "elapsed_ms": 35,
                        "post_state": dict(post_snap_1),
                        "flight_recorder": {},
                        "state_diff": {"windows_opened": []},
                    }
                return {"ok": True}

            runner.rpc = mock.Mock()
            runner.rpc.call.side_effect = fake_rpc_call

            plan_dec = {
                "observation": "In hangar",
                "previous_action_verification": {"expectation_met": True, "discrepancy_summary": ""},
                "bug_report": {"should_file": False, "category": "none", "title": "", "severity": "low", "subsystem": "", "description": "", "reproduction_steps": ""},
                "current_goal": "Open inventory and clear timers",
                "memory_update": {},
                "delegated_plan": {
                    "goal": "Direct 2-step batch",
                    "max_batch_steps": 2,
                    "horizon_rationale": "Routine hangar batch",
                    "success_criteria": "Both actions succeed",
                    "abort_conditions": [],
                    "steps": [
                        {
                            "step_number": 1,
                            "exec_mode": "direct",
                            "capture_screenshot": False,
                            "action_type_hint": "open_neocom",
                            "target_or_command_hint": "inventory",
                            "instruction": "Open Inventory",
                            "expected_outcome": "Inventory opens",
                            "action": {"type": "open_neocom", "target": "inventory", "wait_ms": 250},
                            "assertions": {"expect_window_open": "InventoryStation"},
                        },
                        {
                            "step_number": 2,
                            "exec_mode": "direct",
                            "capture_screenshot": False,
                            "action_type_hint": "slash_command",
                            "target_or_command_hint": "/cleartimers",
                            "instruction": "Clear timers (fails window assertion)",
                            "expected_outcome": "Timers cleared",
                            "action": {"type": "slash_command", "command": "/cleartimers", "wait_ms": 250},
                            "assertions": {"expect_window_open": "NonExistentWindow"},
                        },
                    ],
                },
            }

            cap_calls = []

            def fake_cap(dest_path, **kwargs):
                cap_calls.append(Path(dest_path).name)
                Path(dest_path).write_bytes(b"\x89PNG\r\n\x1a\n")
                return {"ok": True, "mode": "mock"}

            with mock.patch.object(playtester, "capture_window_screenshot", side_effect=fake_cap), \
                 mock.patch.object(playtester, "invoke_agy_planner", return_value=(plan_dec, "hydrogen-l-tf-medium", 1.1)) as m_plan, \
                 mock.patch.object(playtester, "invoke_agy_executor") as m_exec:
                runner.execute_step(1, {}, [])
                # Step 1 skips Executor, uses Planner's routine chain, and caches post_state
                m_plan.assert_called_once()
                self.assertEqual(m_plan.call_args.kwargs["model_chain"], ["hydrogen-l-tf-medium"])
                m_exec.assert_not_called()
                self.assertIsNotNone(runner.cached_post_state)
                cap_calls_after_1 = len(cap_calls)

                # Step 2 reuses cached_post_state (no get_state RPC!), skips pre-action screenshot,
                # fails `expect_window_open="NonExistentWindow"`, captures recovery screenshot, and
                # schedules diagnostic escalation!
                rpc_calls.clear()
                m_plan.reset_mock()
                runner.execute_step(2, {}, [])
                self.assertNotIn("get_state", rpc_calls)
                m_plan.assert_not_called()
                m_exec.assert_not_called()
                self.assertTrue(runner._escalate_to_diagnostic_planner)
                self.assertEqual(len(cap_calls), cap_calls_after_1 + 1)

            runner.oracle.flush(timeout=5.0)
            queue_dir = td_p / ".evomu-bugs" / "queue"
            if queue_dir.exists():
                self.assertTrue(isinstance(list(queue_dir.glob("*.json")), list))


    def test_extract_fenced_json_and_normalize_slim_decisions(self):
        raw_envelope = json.dumps({
            "conversation_id": "conv-slim",
            "status": "SUCCESS",
            "response": "Here is the JSON:\n```json\n" + json.dumps({
                "observation": "In hangar",
                "previous_action_verification": {"expectation_met": True},
                "reasoning": "Open skills cleanly",
                "current_goal": "Verify skills",
                "delegated_plan": {
                    "plan_goal": "Open skills",
                    "horizon_rationale": "6-step batch",
                    "max_batch_steps": 6,
                    "steps": [{"step_number": 1, "exec_mode": "direct", "action": {"type": "open_neocom", "target": "skills"}}],
                    "success_criteria": "Opens",
                    "abort_conditions": ["Error modal"],
                },
                "memory_update": {"working_memory": ["note1"]},
                "bug_report": {"should_file": False},
            }) + "\n```",
        })
        parsed = playtester._extract_json_decision(raw_envelope, required_key="delegated_plan")
        norm = playtester._normalize_planner_decision(parsed)
        self.assertIsNotNone(norm)
        self.assertEqual(norm["bug_report"]["category"], "none")
        self.assertFalse(norm["bug_report"]["should_file"])
        self.assertEqual(norm["delegated_plan"]["steps"][0]["action"]["wait_ms"], 250)
        self.assertEqual(norm["delegated_plan"]["steps"][0]["assertions"], {})

    def test_routine_worker_omits_schema_and_add_dir_with_schema_retry(self):
        spawned_cmds = []

        class FakeWorker:
            def __init__(self, schema_path, model_name, sandbox_dir, enable_sandbox_files=False):
                spawned_cmds.append((schema_path, model_name, enable_sandbox_files))

            def run_turn(self, prompt_text, timeout_secs=90.0):
                if len(spawned_cmds) == 1:
                    return 0, json.dumps({"response": "oops not json"}), "", 0.2
                return 0, json.dumps({
                    "structured_output": {
                        "observation": "ok",
                        "previous_action_verification": {"expectation_met": True, "discrepancy_summary": ""},
                        "reasoning": "ok",
                        "current_goal": "g",
                        "delegated_plan": {
                            "plan_goal": "g",
                            "horizon_rationale": "h",
                            "max_batch_steps": 1,
                            "steps": [{"step_number": 1, "exec_mode": "direct", "action": {"type": "clear_timers"}}],
                            "success_criteria": "ok",
                            "abort_conditions": "err",
                        },
                        "memory_update": {},
                        "bug_report": {"should_file": False},
                    }
                }), "", 0.3

            def terminate(self):
                pass

        with tempfile.TemporaryDirectory() as td:
            td_p = Path(td)
            pool = playtester.AgyStreamWorkerPool(sandbox_dir=td_p / "sb", standby_specs=[], auto_respawn=False)
            with mock.patch.object(playtester, "_WarmAgyWorker", side_effect=FakeWorker):
                dec, model, _ = playtester.invoke_agy_planner(
                    "prompt",
                    td_p / "step1",
                    ["hydrogen-l-tf-medium"],
                    stream_pool=pool,
                    diagnostic=False,
                )
            self.assertEqual(model, "hydrogen-l-tf-medium")
            self.assertEqual(len(spawned_cmds), 2)
            # First call omits both schema_path and enable_sandbox_files; retry enables schema fallback
            self.assertIsNone(spawned_cmds[0][0])
            self.assertFalse(spawned_cmds[0][2])
            self.assertEqual(spawned_cmds[1][0], playtester.PLANNER_SCHEMA_PATH)
            self.assertFalse(spawned_cmds[1][2])
            pool.shutdown()

            # Check actual _WarmAgyWorker cmd construction via Popen mock
            popen_cmds = []

            class DummyP:
                stdin = stdout = stderr = None
                def poll(self): return None
                def terminate(self): pass
                def wait(self, timeout=None): return 0

            with mock.patch.object(playtester.subprocess, "Popen", side_effect=lambda cmd, **kw: popen_cmds.append(cmd) or DummyP()), \
                 mock.patch.object(playtester._WarmAgyWorker, "_wait_for_init", return_value=None):
                playtester._WarmAgyWorker(None, "hydrogen-l-tf-medium", td_p / "sb", enable_sandbox_files=False)
                playtester._WarmAgyWorker(playtester.PLANNER_SCHEMA_PATH, "hydrogen-l-tf-high", td_p / "sb", enable_sandbox_files=True)
            self.assertNotIn("--json-schema", popen_cmds[0])
            self.assertNotIn("--add-dir", popen_cmds[0])
            self.assertIn("--json-schema", popen_cmds[1])
            self.assertIn("--add-dir", popen_cmds[1])

    def test_oracle_lazy_screenshot_captures_on_server_error(self):
        with tempfile.TemporaryDirectory() as td:
            td_p = Path(td)
            mem = playtester.PlaytesterMemory(td_p / "state", git_head="deadbeef")
            oracle = playtester.PlaytesterBugOracle(td_p, td_p / "q", mem, sync_github=False, async_write=False)
            tb_log = td_p / "tb.log"
            srv_log = td_p / "srv.log"
            tb_log.write_text("", encoding="utf-8")
            srv_log.write_text("ERROR eve_server::router: test failure\n", encoding="utf-8")
            shot = td_p / "screen.png"
            called = []

            def lazy_cap():
                called.append(True)
                shot.write_bytes(b"\x89PNG\r\n\x1a\n")

            created = oracle.evaluate_step(
                step_num=99,
                git_head="deadbeef",
                decision={"action": {"type": "clear_timers"}, "observation": "o", "reasoning": "r"},
                exec_res={"action_ok": True, "flight_recorder": {}},
                ui_snapshot={"active_view": "hangar", "session": {}},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb_log,
                server_log=srv_log,
                step_dir=td_p,
                last_flight_recorder={},
                lazy_screenshot_fn=lazy_cap,
            )
            self.assertEqual(len(created), 1)
            self.assertEqual(called, [True])
            self.assertTrue(shot.exists())


    def test_click_alias_and_unknown_atype_rejected(self):
        self.assertEqual(
            playtester._normalize_action_defaults({"type": "click", "target": "repairshop"})["type"],
            "click_widget",
        )
        self.assertEqual(
            playtester._normalize_action_defaults({"type": "click", "x": 100, "y": 200})["type"],
            "click_xy",
        )
        self.assertEqual(playtester._normalize_action_defaults({"type": "drag"})["type"], "drag_widget")
        self.assertEqual(playtester._normalize_action_defaults({"type": "drag_and_drop"})["type"], "drag_widget")
        self.assertEqual(playtester._normalize_action_defaults({"type": "type"})["type"], "type_text")
        self.assertEqual(playtester._normalize_action_defaults({"type": "input_text"})["type"], "type_text")
        self.assertEqual(playtester._normalize_action_defaults({"type": "enter_text"})["type"], "type_text")
        for alias in ("key_press", "hotkey", "press_hotkey", "shortcut"):
            self.assertEqual(playtester._normalize_action_defaults({"type": alias})["type"], "press_key")


    def test_benign_user_error_markers_for_boarding_lance_cooldown(self):
        for msg in (
            "UserError('CustomInfo', {'info': 'ShipOutOfBoardingRange: Target ship 1 is 5503.2 m away'})",
            "UserError('CustomInfo', {'info': 'LanceFiringLockActive: Cannot warp while lock active'})",
            "UserError('CustomInfo', {'info': 'ModuleInReactivationDelay: Module 1 is in reactivation delay'})",
            "UserError('CustomInfo', {'info': 'IncompatibleAmmo: Charge EMP S is incompatible with weapon Light Ion Blaster I'})",
            "UserError('FittingHasSkillPrerequisites', {})",
            "UserError('TargetNotLocked', {})",
        ):
            self.assertTrue(
                playtester.PlaytesterBugOracle.is_expected_user_error({"error": msg}, ""),
                f"Expected benign marker match for {msg}",
            )

    def test_benign_user_error_markers_target_too_far_warp_gone_and_window_blocked(self):
        oracle = playtester.PlaytesterBugOracle
        for marker in ("targettoofar", "warpdestinationgone", "notcloseenoughtojump", "windowblockedfromopening"):
            self.assertIn(marker, oracle.BENIGN_USER_ERROR_MARKERS)

        cases = (
            (
                "NotCloseEnoughToJump",
                {"error": "UserError('NotCloseEnoughToJump', {'maxdist': 2500})"},
                "UserError: NotCloseEnoughToJump",
                (
                    "  File \"eve/client/script/ui/services/autoPilot.py\", line 412, in StargateJump\n"
                    "UserError: User error, msg=NotCloseEnoughToJump, dict={'maxdist': 2500}\n"
                ),
            ),
            (
                "TargetTooFar",
                {"error": "UserError('CustomInfo', {'info': 'TargetTooFar: Target is out of range'})"},
                "UserError: TargetTooFar",
                (
                    "  File \"eve/client/script/ui/inflight/shipModuleButton/shipmodulebutton.py\", line 1580, in ActivateEffect\n"
                    "UserError: User error, msg=CustomInfo, dict={'info': 'TargetTooFar'}\n"
                ),
            ),
            (
                "WarpDestinationGone",
                {"error": "UserError('WarpDestinationGone', {})"},
                "NotifyOnWarpCancelled",
                (
                    "  File \"eve/client/script/ui/services/menuSvc.py\", line 1120, in WarpToItem\n"
                    "UserError: User error, msg=WarpDestinationGone, dict={}\n"
                ),
            ),
            (
                "WindowBlockedFromOpening",
                {"error": "UserError('WindowBlockedFromOpening', {'window_name': u'Inventory'})"},
                "Window.Open",
                (
                    "  File \"carbonui/control/window.py\", line 410, in Open\n"
                    "UserError: User error, msg=WindowBlockedFromOpening, dict={'window_name': u'Inventory'}\n"
                ),
            ),
        )
        for label, err_obj, headline, trace in cases:
            self.assertTrue(
                oracle.is_expected_user_error(err_obj, ""),
                f"Expected is_expected_user_error=True for {label}",
            )
            self.assertTrue(
                oracle.is_ignorable_client_traceback(headline, trace),
                f"Expected is_ignorable_client_traceback=True for {label}",
            )

    def test_load_pending_verifications_accepts_pending_verification_status(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            td = Path(tmpdir)
            (td / "pending_verifications.json").write_text(
                json.dumps({
                    "items": [
                        {"issue": 1031, "status": "pending_verification", "title": "t1"},
                        {"issue": 1032, "status": "pending", "title": "t2"},
                        {"issue": 1000, "status": "verified_fixed", "title": "done"},
                    ]
                })
            )
            loaded = playtester.load_pending_verifications(td)
        self.assertEqual([x["issue"] for x in loaded], [1031, 1032])

    def test_wait_action_resolves_numeric_target_over_default_wait_ms(self):
        norm = playtester._normalize_action_defaults(
            {"type": "wait", "target": "5000", "wait_ms": 250}
        )
        self.assertEqual(norm["wait_ms"], 5000)

        direct, reason = playtester._resolve_direct_action(
            {"action": {"type": "wait", "target": "wait 2500", "wait_ms": 250}},
            ui_snapshot={},
        )
        self.assertEqual(reason, "")
        self.assertEqual(direct["wait_ms"], 2500)

        explicit = playtester._normalize_action_defaults(
            {"type": "wait", "target": "1500", "wait_ms": 4000}
        )
        self.assertEqual(explicit["wait_ms"], 4000)


    def test_warm_agy_worker_does_not_hang_on_grandchild_holding_stderr(self):
        import subprocess
        import threading
        import time

        script = (
            "import json, subprocess, sys, time; "
            "print(json.dumps({'event': 'init', 'conversation_id': 'c1'}), flush=True); "
            "_ = sys.stdin.readline(); "
            "subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)']); "
            "print('child-stderr-open', file=sys.stderr, flush=True); "
            "print(json.dumps({'event': 'result', 'result': {'ok': True}}), flush=True)"
        )
        worker = object.__new__(playtester._WarmAgyWorker)
        worker.schema_path = None
        worker.model_name = "test"
        worker.sandbox_dir = Path("/tmp")
        worker.enable_sandbox_files = False
        worker.proc = subprocess.Popen(
            [sys.executable, "-c", script],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
            start_new_session=True,
        )
        worker.init_event = threading.Event()
        worker.init_error = ""
        worker.conversation_id = ""
        worker._buf = bytearray()
        worker._warm_thread = threading.Thread(target=worker._wait_for_init, daemon=True)
        worker._warm_thread.start()
        t0 = time.time()
        try:
            rc, raw_out, stderr_s, _ = worker.run_turn("hello", timeout_secs=5.0)
        finally:
            worker.terminate()
        self.assertLess(time.time() - t0, 3.0)
        self.assertEqual(rc, 0)
        self.assertIn("child-stderr-open", stderr_s)
        self.assertEqual(json.loads(raw_out), {"ok": True})


    def test_evaluate_step_suppresses_benign_inventory_not_accessible_agent_bug_report(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            tb = root / "client_tracebacks.log"
            srv = root / "server.log"
            shot = root / "screen.png"
            sdir = root / "step"
            qdir.mkdir()
            sdir.mkdir()
            tb.write_text("", encoding="utf-8")
            srv.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")

            mem = playtester.PlaytesterMemory(root, git_head="66c9ccfe7")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            decision = {
                "observation": "Batch #208 Step 1",
                "previous_action_verification": {"expectation_met": True, "discrepancy_summary": ""},
                "action": {"type": "slash_command", "target": "/fit me 563", "command": "/fit me 563"},
                "bug_report": {
                    "should_file": True,
                    "category": "server_rpc_error",
                    "title": "invbroker.GetInventoryFromId raises InventoryNotAccessible for stale item 1897148069 on docking at Jita 4-4",
                    "severity": "low",
                    "subsystem": "inventory",
                    "expected_behavior": "Open station hangar without stale IDs",
                    "actual_behavior": (
                        "invbroker.GetInventoryFromId(1897148069, 0) raised "
                        "UserError('CustomInfo', {'info': 'InventoryNotAccessible: item 1897148069 is not an inventory reachable'})"
                    ),
                    "reproduction_steps": ["Dock at Jita 4-4"],
                },
            }
            last_fr = {
                "log_exceptions": [],
                "quasar_errors": [],
                "macho_exceptions": [
                    {
                        "service": "invbroker",
                        "method": "GetInventoryFromId",
                        "args": "(1897148069, 0)",
                        "error": "UserError('CustomInfo', {'info': 'InventoryNotAccessible: item 1897148069 is not an inventory reachable'})",
                    }
                ],
            }
            queued = oracle.evaluate_step(
                step_num=36225,
                git_head="66c9ccfe7",
                decision=decision,
                exec_res={"action_ok": True, "flight_recorder": {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []}},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder=last_fr,
            )
            self.assertEqual(queued, [])

    def test_build_reasoning_prompt_moves_benign_macho_exceptions_out_of_hard_list(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            mem = playtester.PlaytesterMemory(root, git_head="66c9ccfe7")
            shot = root / "screen.png"
            shot.write_bytes(b"png")
            raw_fr = {
                "log_exceptions": [],
                "quasar_errors": [],
                "macho_exceptions": [
                    {
                        "service": "invbroker",
                        "method": "GetInventoryFromId",
                        "error": "UserError('CustomInfo', {'info': 'InventoryNotAccessible: stale item'})",
                    }
                ],
            }
            prompt = playtester.build_reasoning_prompt(
                step_num=36225,
                screenshot_path=shot,
                burst_paths=[],
                ui_snapshot={},
                last_flight_recorder=raw_fr,
                memory=mem,
                git_head="66c9ccfe7",
            )
            self.assertEqual(len(raw_fr["macho_exceptions"]), 1)
            self.assertIn('"benign_macho_exceptions":', prompt)
            self.assertIn('"macho_exceptions":[]', prompt.replace(" ", ""))

    def test_evaluate_step_flags_destiny_stale_at_action_via_classify_client_log(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            run_dir = root / "run"
            sdir = run_dir / "steps" / "step_0010"
            clogs = run_dir / "client_logs"
            qdir.mkdir(parents=True)
            sdir.mkdir(parents=True)
            clogs.mkdir(parents=True)
            tb = run_dir / "client_tracebacks.log"
            srv = run_dir / "server.log"
            shot = sdir / "screen.png"
            tb.write_text("", encoding="utf-8")
            srv.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")
            (clogs / "destiny.log").write_text(
                "DoDestinyUpdate call for tick 100 containing 1 updates. waitForBubble=0\n"
                "Action: SetBallPosition 95 - current: 100 (1897000001L, 0.0, 0.0, 0.0)\n",
                encoding="utf-8",
            )

            mem = playtester.PlaytesterMemory(root, git_head="2306bfa74")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            queued = oracle.evaluate_step(
                step_num=10,
                git_head="2306bfa74",
                decision={"action": {"type": "warp_to", "target": "60003760"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": True,
                    "flight_recorder": {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                    "warp_telemetry": {"saw_mode3": True, "saw_fx_warping": True, "warp_signal_count": 1},
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(queued), 1)
            payload = json.loads(queued[0].read_text(encoding="utf-8"))
            self.assertEqual(payload["kind"], "destiny_stale_stamp")

    def test_benign_user_error_markers_include_cannot_unfit_in_space(self):
        self.assertTrue(
            playtester.PlaytesterBugOracle.is_expected_user_error(
                "UserError('CustomInfo', {'info': 'CannotUnfitInSpace: Unfitting modules requires being docked'})"
            )
        )
        self.assertTrue(
            playtester.PlaytesterBugOracle.is_expected_user_error(
                "UserError('CustomInfo', {'info': 'CannotFitInSpace: Fitting requires station or depot'})"
            )
        )
        self.assertTrue(
            playtester.PlaytesterBugOracle.is_expected_user_error(
                "UserError('CustomInfo', {'info': 'TooCloseToStructureOrStation: Anchoring position violates minimum clearance'})"
            )
        )
        self.assertTrue(
            playtester.PlaytesterBugOracle.is_expected_user_error(
                "UserError('CustomInfo', {'info': 'SessionChangeTimerActive: Session change timer is active'})"
            )
        )
        self.assertTrue(
            playtester.PlaytesterBugOracle.is_expected_user_error(
                "UserError('CustomInfo', {'info': \"StructureNotOperational: Structure 1001897205354 is in state 'ShieldVulnerable' (cored=false) and cannot be controlled\"})"
            )
        )
        self.assertTrue(
            playtester.PlaytesterBugOracle.is_expected_user_error(
                "UserError('CustomInfo', {'info': 'KeepstarPerSystemLimit: Only one Keepstar-sized Citadel may be anchored in solar system 30004759'})"
            )
        )

    def test_benign_user_error_markers_include_quantum_core_dockable_and_warp_short(self):
        oracle_cls = playtester.PlaytesterBugOracle

        def _eval_macho(svc: str, meth: str, err_str: str, expected_ui: str, action_error: str):
            with tempfile.TemporaryDirectory() as td:
                root = Path(td)
                qdir = root / "queue"
                sdir = root / "step_1"
                qdir.mkdir()
                sdir.mkdir()
                shot = root / "shot.png"
                shot.write_bytes(b"png")
                tb = root / "tb.log"
                tb.write_text("", encoding="utf-8")
                srv = root / "srv.log"
                srv.write_text("", encoding="utf-8")
                mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
                oracle = oracle_cls(root, qdir, mem, sync_github=False)
                queued = oracle.evaluate_step(
                    step_num=1,
                    git_head="deadbeef",
                    decision={
                        "action": {"type": "click_widget", "target": "ok", "expected_ui_and_state_change": expected_ui},
                        "bug_report": {"should_file": False},
                    },
                    exec_res={
                        "action_ok": False,
                        "action_error": action_error,
                        "flight_recorder": {
                            "log_exceptions": [],
                            "quasar_errors": [],
                            "macho_exceptions": [{"service": svc, "method": meth, "error": err_str}],
                        },
                    },
                    ui_snapshot={},
                    screenshot_path=shot,
                    burst_paths=[],
                    traceback_log=tb,
                    server_log=srv,
                    step_dir=sdir,
                )
                return [json.loads(p.read_text(encoding="utf-8")) for p in queued]

        benign_cases = (
            (
                "N=888444:10057",
                "MultiAdd",
                "UserError('CustomInfo', {'info': 'QuantumCoreRequired: Ship fitting service is disabled at structure 1001897528498 until its Quantum Core is online'})",
                "Miner I fits into high power slot",
            ),
            (
                "stationSvc",
                "Dock",
                "UserError('CustomInfo', {'info': \"StructureNotDockable: Structure 1001897542825 is in state 'Anchoring' and cannot be docked at\"})",
                "Ship docks at Upwell structure",
            ),
            (
                "beyonce",
                "WarpToObject",
                "UserError('WarpTooShort', {})",
                "Ship warps to celestial object",
            ),
            (
                "structureDocking",
                "Dock",
                "UserError('StructureNotDockable', {})",
                "Ship docks at structure",
            ),
            (
                "repairSvc",
                "RepairItemsInStructure",
                "UserError('QuantumCoreRequired', {})",
                "Items repaired in structure",
            ),
            (
                "structureSettings",
                "CharacterCheckService",
                "UserError('CustomInfo', {'info': 'StructureServiceAccessDenied: Character is denied structure service access'})",
                "Access structure service",
            ),
            (
                "structureSettings",
                "CharacterCheckService",
                "UserError('StructureServiceAccessDenied', {})",
                "Access structure service",
            ),
        )
        for svc, meth, err_str, expected_ui in benign_cases:
            with self.subTest(err=err_str):
                self.assertTrue(oracle_cls.is_expected_user_error(err_str, expected_ui))
                self.assertTrue(
                    oracle_cls.is_expected_user_error({"service": svc, "method": meth, "error": err_str}, expected_ui)
                )
                self.assertEqual(
                    _eval_macho(svc, meth, err_str, expected_ui, f"Client action rejected with {err_str}"),
                    [],
                )

        # Non-UserError failures (even mentioning the marker token) and unknown UserErrors still queue protocol_error.
        non_benign_cases = (
            ("stationSvc", "Dock", "RuntimeError('StructureNotDockable')", "Ship docks at structure"),
            ("structureSettings", "CharacterCheckService", "RuntimeError('StructureServiceAccessDenied')", "Access structure service"),
            ("beyonce", "WarpToObject", "UserError('CustomInfo', {'info': 'UnexpectedWarpFailure'})", "Ship warps"),
        )
        for svc, meth, err_str, expected_ui in non_benign_cases:
            with self.subTest(non_benign=err_str):
                self.assertFalse(oracle_cls.is_expected_user_error({"service": svc, "method": meth, "error": err_str}, expected_ui))
                payloads = _eval_macho(svc, meth, err_str, expected_ui, err_str)
                self.assertEqual(len(payloads), 1)
                self.assertEqual(payloads[0]["kind"], "protocol_error")

    def test_type_text_preserves_text_alias_in_executor_and_direct_resolution(self):
        dec = playtester._normalize_executor_decision(
            {
                "observation": "Inventory open",
                "action": {
                    "type": "type_text",
                    "target": "quickFilterInputBox",
                    "text": "Caldari Shuttle\n",
                },
            }
        )
        self.assertEqual(dec["action"]["command"], "Caldari Shuttle\n")

        act, reason = playtester._resolve_direct_action(
            {
                "action": {
                    "type": "type_text",
                    "target": "quickFilterInputBox",
                    "text": "Tritanium\n",
                }
            },
            {
                "ui": {
                    "clickable_widgets": [
                        {"name": "quickFilterInputBox", "path": "InventoryStation/quickFilterInputBox"}
                    ]
                }
            },
        )
        self.assertEqual(reason, "")
        self.assertEqual(act["command"], "Tritanium\n")

        drag_dec = playtester._normalize_executor_decision(
            {
                "observation": "Core room open",
                "action": {
                    "type": "drag_and_drop",
                    "target": "ItemEntry_56203",
                    "dest_x": 850,
                    "dest_y": 500,
                },
            }
        )
        self.assertEqual(drag_dec["action"]["type"], "drag_widget")


    def test_evaluate_step_captures_unhandled_tasklet_shipinwarp_traceback(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            tb = root / "client_tracebacks.log"
            srv = root / "server.log"
            shot = root / "screen.png"
            sdir = root / "step"
            qdir.mkdir()
            sdir.mkdir()
            tb.write_text("", encoding="utf-8")
            srv.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")

            mem = playtester.PlaytesterMemory(root, git_head="7fa41edb3")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            tasklet_tb = (
                "logmodule.LogException args=('Unhandled exception in <TaskletExt object at 0xca370d558, abps=1001, ctxt=None>',)\n"
                "Traceback (most recent call last):\n"
                "  File \"autopilot.py\", line 628, in __NavigateSystemTo\n"
                "  File \"structureDocking.py\", line 68, in RequestDocking\n"
                "UserError: User error, msg=ShipInWarp, dict={}"
            )
            exec_res = {
                "action_ok": True,
                "flight_recorder": {
                    "log_exceptions": [
                        {
                            "headline": "UserError: User error, msg=ShipInWarp, dict={}",
                            "trace": tasklet_tb,
                        }
                    ],
                    "quasar_errors": [],
                    "macho_exceptions": [],
                },
            }
            queued = oracle.evaluate_step(
                step_num=44142,
                git_head="7fa41edb3",
                decision={"action": {"type": "press_key", "target": "CTRL+SPACE"}, "bug_report": {"should_file": False}},
                exec_res=exec_res,
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(len(queued), 1)
            bundle = json.loads(queued[0].read_text(encoding="utf-8"))
            self.assertEqual(bundle["kind"], "client_traceback")

            # Benign BluepyTasklet::_MoveItems NotEnoughCargoSpace must still be filtered
            self.assertFalse(
                oracle._is_unhandled_tasklet_traceback(
                    "UserError: User error, msg=NotEnoughCargoSpace, dict={'volume': 800000.0, 'available': 0.0}",
                    "Unhandled exception in <TaskletExt object at 0x1, ctxt='BluepyTasklet::_MoveItems'>\ninvWindow.py",
                )
            )

    def test_evaluate_step_does_not_suppress_agent_bug_when_prev_channel1_skipped(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            tb = root / "client_tracebacks.log"
            srv = root / "server.log"
            shot = root / "screen.png"
            sdir = root / "step"
            qdir.mkdir()
            sdir.mkdir()
            tb.write_text("", encoding="utf-8")
            srv.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")

            mem = playtester.PlaytesterMemory(root, git_head="7fa41edb3")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            # Step 1: benign-looking log_exception filtered by Channel 1 -> 0 incidents
            prev_log_excs = [{"headline": "UserError: User error, msg=ShipInWarp, dict={}"}]
            q1 = oracle.evaluate_step(
                step_num=44142,
                git_head="7fa41edb3",
                decision={"action": {"type": "wait", "target": "1500"}, "bug_report": {"should_file": False}},
                exec_res={"action_ok": True, "flight_recorder": {"log_exceptions": prev_log_excs, "quasar_errors": [], "macho_exceptions": []}},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(q1, [])
            # Step 2: Agent explicitly files client_exception -> must NOT be suppressed since Step 1 filed nothing.
            dec2 = {
                "action": {"type": "wait", "target": "1500"},
                "previous_action_verification": {"expectation_met": False, "discrepancy_summary": "ShipInWarp desync"},
                "bug_report": {
                    "should_file": True,
                    "category": "client_exception",
                    "severity": "medium",
                    "subsystem": "space-destiny-warp",
                    "title": "Autopilot RequestDocking ShipInWarp + CmdStop ShipAlreadyInWarp 6s after warp exit",
                    "expected_behavior": "Server exits warp when client exits warp",
                    "actual_behavior": "Server stays in warp 6s past client warp exit",
                    "reproduction_steps": ["Warp 3.95 AU via Dock on structure"],
                },
            }
            q2 = oracle.evaluate_step(
                step_num=44143,
                git_head="7fa41edb3",
                decision=dec2,
                exec_res={"action_ok": True, "flight_recorder": {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []}},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": prev_log_excs, "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(q2), 1)

    def test_autoreload_cannot_load_not_enough_charges_is_benign_while_item_not_found_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            qdir.mkdir()
            mem = playtester.PlaytesterMemory(root, git_head="9c1e2fca2")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            benign_tb = (
                'logmodule.LogException args=("Unhandled exception in <TaskletExt object at 0x1, ctxt=\'ModuleController.AutoReload\'>",)\n'
                '  File "packages/hud/client/moduleController.py", line 878, in AutoReload\n'
                '  File "eve/client/script/dogma/clientDogmaLocation.py", line 1184, in LoadAmmoTypeToModule\n'
                '  File "eve/client/script/dogma/clientDogmaLocation.py", line 1193, in LoadChargesToModule\n'
                'UserError: User error, msg=CannotLoadNotEnoughCharges, dict=None\n'
            )
            self.assertFalse(
                oracle._is_unhandled_tasklet_traceback(
                    "UserError: User error, msg=CannotLoadNotEnoughCharges, dict=None",
                    benign_tb,
                )
            )
            item_not_found_tb = benign_tb.replace("CannotLoadNotEnoughCharges", "ItemNotFound")
            self.assertTrue(
                oracle._is_unhandled_tasklet_traceback(
                    "UserError: User error, msg=ItemNotFound, dict={}",
                    item_not_found_tb,
                )
            )

    def test_oracle_filters_noammoloaded_minimap_resizeslider_and_fitting_validation_failed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            qdir.mkdir()
            mem = playtester.PlaytesterMemory(root, git_head="7e3600c29")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            no_ammo_tb = (
                "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x2, ctxt='ModuleButton.OnStateChange'>\",)\n"
                '  File "packages/hud/client/moduleButton.py", line 412, in OnStateChange\n'
                '  File "eve/client/script/environment/spaceObject/module.py", line 118, in ActivateEffect\n'
                "UserError: User error, msg=NoAmmoLoaded, dict={}\n"
            )
            self.assertIn("noammoloaded", oracle.BENIGN_USER_ERROR_MARKERS)
            self.assertFalse(oracle._is_unhandled_tasklet_traceback("UserError: NoAmmoLoaded", no_ammo_tb))
            self.assertTrue(oracle.is_ignorable_client_traceback("Unhandled exception in ModuleButton.OnStateChange", no_ammo_tb))

            minimap_tb = (
                '  File "packages/map/client/minimap.py", line 219, in _ResizeSlider\n'
                "AttributeError: 'MapSlider' object has no attribute 'minimumWidth'\n"
            )
            self.assertTrue(oracle.is_benign_client_traceback("'MapSlider' object has no attribute 'minimumWidth'", minimap_tb))
            self.assertTrue(oracle.is_ignorable_client_traceback("AttributeError", minimap_tb))

            fitting_drag_tb = (
                "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x3, ctxt='fittingSlotController.OnDropData'>\",)\n"
                '  File "eve/client/script/ui/shared/fitting/fittingSlotController.py", line 310, in OnDropData\n'
                '  File "eve/client/script/inc/invCache.py", line 915, in TryFit\n'
                "UserError: ('CustomInfo', {'info': 'FittingValidationFailed: No available turret hardpoints'})\n"
            )
            self.assertFalse(oracle._is_unhandled_tasklet_traceback("Unhandled exception in OnDropData", fitting_drag_tb))
            self.assertTrue(oracle.is_ignorable_client_traceback("Unhandled exception in OnDropData", fitting_drag_tb))

            unexpected_custom_info_tb = (
                "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x4, ctxt='market.Buy'>\",)\n"
                '  File "eve/client/script/ui/shared/market/marketWindow.py", line 210, in ConfirmOrder\n'
                "UserError: ('CustomInfo', {'info': 'UnexpectedInternalServerMismatch'})\n"
            )
            self.assertTrue(oracle._is_unhandled_tasklet_traceback("Unhandled exception in market.Buy", unexpected_custom_info_tb))
            self.assertFalse(oracle.is_ignorable_client_traceback("Unhandled exception in market.Buy", unexpected_custom_info_tb))

            # GH #1227: invWindow.py in _MoveItems tasklet UserError(NotEnoughCargoSpace)
            inv_move_tb = (
                "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x5, ctxt='invWindow._MoveItems'>\",)\n"
                '  File "eve/client/script/ui/shared/inventory/invWindow.py", line 812, in _MoveItems\n'
                "UserError: User error, msg=NotEnoughCargoSpace, dict={'space': 5.0}\n"
            )
            self.assertFalse(oracle._is_unhandled_tasklet_traceback("Unhandled exception in invWindow._MoveItems", inv_move_tb))
            self.assertTrue(oracle.is_ignorable_client_traceback("Unhandled exception in invWindow._MoveItems", inv_move_tb))

            inv_move_crash_tb = (
                "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x6, ctxt='invWindow._MoveItems'>\",)\n"
                '  File "eve/client/script/ui/shared/inventory/invWindow.py", line 812, in _MoveItems\n'
                "RuntimeError: unexpected UI container crash\n"
            )
            self.assertTrue(oracle._is_unhandled_tasklet_traceback("Unhandled exception in invWindow._MoveItems", inv_move_crash_tb))
            self.assertFalse(oracle.is_ignorable_client_traceback("Unhandled exception in invWindow._MoveItems", inv_move_crash_tb))

            # GH #1230 / #1231: ModuleButton.OnStateChange -> ActivateEffect tasklet UserError(InsufficientFuel / NotEnoughCapacitor)
            for msg_marker in (
                "InsufficientFuel: Insufficient superweapon fuel (type 16274): need 50000, have 30000",
                "NotEnoughCapacitor: Insufficient capacitor for Doomsday: need 35000.0 GJ",
            ):
                doomsday_tb = (
                    "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x7, ctxt='ModuleButton.OnStateChange'>\",)\n"
                    '  File "eve/client/script/ui/inflight/shipModuleButton/shipmodulebutton.py", line 1630, in OnStateChange\n'
                    '  File "eve/client/script/ui/inflight/shipModuleButton/shipmodulebutton.py", line 1580, in ActivateEffect\n'
                    f"UserError: User error, msg=CustomInfo, dict={{'info': '{msg_marker}'}}\n"
                )
                self.assertFalse(oracle._is_unhandled_tasklet_traceback("Unhandled exception in ModuleButton.OnStateChange", doomsday_tb))
                self.assertTrue(oracle.is_ignorable_client_traceback("Unhandled exception in ModuleButton.OnStateChange", doomsday_tb))

            doomsday_unexpected_tb = (
                "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x8, ctxt='ModuleButton.OnStateChange'>\",)\n"
                '  File "eve/client/script/ui/inflight/shipModuleButton/shipmodulebutton.py", line 1630, in OnStateChange\n'
                '  File "eve/client/script/ui/inflight/shipModuleButton/shipmodulebutton.py", line 1580, in ActivateEffect\n'
                "UserError: User error, msg=CustomInfo, dict={'info': 'UnexpectedDoomsdayState'}\n"
            )
            self.assertTrue(oracle._is_unhandled_tasklet_traceback("Unhandled exception in ModuleButton.OnStateChange", doomsday_unexpected_tb))
            self.assertFalse(oracle.is_ignorable_client_traceback("Unhandled exception in ModuleButton.OnStateChange", doomsday_unexpected_tb))

    def test_evemisc_raise_drone_limit_user_error_is_benign_while_unexpected_errors_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            qdir.mkdir()
            mem = playtester.PlaytesterMemory(root, git_head="7692e11ae")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            drone_cap_tb = (
                "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x95da80dd8, abps=1001, ctxt='<NO CONTEXT>^<function raise_ ff248588>'>\",) kwargs={}\n"
                "Traceback (most recent call last):\n"
                '  File "packages/bluepy/__init__.py", line 55, in CallWrapper\n'
                '  File "eve/client/script/util/eveMisc.py", line 105, in raise_\n'
                "UserError: User error, msg=NoDroneManagementAbilitiesLeft, dict=None\n"
            )
            self.assertFalse(
                oracle._is_unhandled_tasklet_traceback(
                    "UserError: User error, msg=NoDroneManagementAbilitiesLeft, dict=None",
                    drone_cap_tb,
                )
            )
            self.assertTrue(
                oracle.is_ignorable_client_traceback(
                    "UserError: User error, msg=NoDroneManagementAbilitiesLeft, dict=None",
                    drone_cap_tb,
                    "Context menu closes and Hobgoblin I drones are launched into space.",
                )
            )

            bay_space_tb = drone_cap_tb.replace(
                "NoDroneManagementAbilitiesLeft", "NotEnoughDroneBaySpace"
            )
            self.assertFalse(
                oracle._is_unhandled_tasklet_traceback(
                    "UserError: User error, msg=NotEnoughDroneBaySpace, dict=None",
                    bay_space_tb,
                )
            )
            self.assertTrue(
                oracle.is_ignorable_client_traceback(
                    "UserError: User error, msg=NotEnoughDroneBaySpace, dict=None",
                    bay_space_tb,
                )
            )

            unexpected_user_error_tb = drone_cap_tb.replace(
                "NoDroneManagementAbilitiesLeft", "ItemNotFound"
            )
            self.assertTrue(
                oracle._is_unhandled_tasklet_traceback(
                    "UserError: User error, msg=ItemNotFound, dict=None",
                    unexpected_user_error_tb,
                )
            )
            self.assertFalse(
                oracle.is_ignorable_client_traceback(
                    "UserError: User error, msg=ItemNotFound, dict=None",
                    unexpected_user_error_tb,
                )
            )

            other_site_tb = drone_cap_tb.replace(
                'File "eve/client/script/util/eveMisc.py", line 105, in raise_',
                'File "eve/client/script/parklife/droneMgr.py", line 210, in OnCrash',
            ).replace("function raise_", "function OnCrash")
            self.assertTrue(
                oracle._is_unhandled_tasklet_traceback(
                    "UserError: User error, msg=NoDroneManagementAbilitiesLeft, dict=None",
                    other_site_tb,
                )
            )
            self.assertFalse(
                oracle.is_ignorable_client_traceback(
                    "UserError: User error, msg=NoDroneManagementAbilitiesLeft, dict=None",
                    other_site_tb,
                )
            )

    def test_parse_tb_blocks_splits_action_tasklet_and_schedule_startup_headers(self):
        raw_tb = (
            "[evomu_site] logmodule.LogException args=(\"first\",)\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/foo.py", line 10, in Foo\n'
            "RuntimeError: first_err\n"
            "[evomu_site] Action tasklet exception (dock):\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/bar.py", line 20, in Dock\n'
            "RuntimeError: action_err\n"
            "[evomu_site] schedule_startup failed:\n"
            "Traceback (most recent call last):\n"
            '  File "evomu_site.py", line 1347, in _schedule\n'
            "RuntimeError: startup_err\n"
        )
        blocks = playtester.PlaytesterBugOracle.parse_tb_blocks(raw_tb)
        self.assertEqual(len(blocks), 3, blocks)
        self.assertIn("first_err", blocks[0]["headline"] + blocks[0]["trace"])
        self.assertNotIn("Action tasklet exception", blocks[0]["trace"])
        self.assertIn("action_err", blocks[1]["headline"] + blocks[1]["trace"])
        self.assertIn("Action tasklet exception", blocks[1]["trace"])
        self.assertNotIn("schedule_startup failed:", blocks[1]["trace"])
        self.assertIn("startup_err", blocks[2]["headline"] + blocks[2]["trace"])
        self.assertIn("schedule_startup failed:", blocks[2]["trace"])

    def test_abort_conditions_schema_string_parity(self):
        schema = json.loads((HARNESS_DIR / "playtester_planner_schema.json").read_text(encoding="utf-8"))
        abort_schema = schema["properties"]["delegated_plan"]["properties"]["abort_conditions"]
        self.assertEqual(abort_schema["type"], "string")

        fb = playtester._heuristic_fallback_planner_decision()
        self.assertIsInstance(fb["delegated_plan"]["abort_conditions"], str)
        self.assertTrue(fb["delegated_plan"]["abort_conditions"])

        with tempfile.TemporaryDirectory() as td:
            mem = playtester.PlaytesterMemory(Path(td), git_head="deadbeef")
            pp = playtester.build_planner_prompt(
                1, Path(td) / "screen.png", [], {"active_view": "hangar"}, {}, mem, "deadbeef",
                use_compact_json_contract=True,
            )
        self.assertIn('"abort_conditions": "Error modal"', pp)
        self.assertNotIn('"abort_conditions": ["Error modal"]', pp)



def _parse_simple_toml(text: str) -> dict:
    """Minimal TOML subset parser for Python < 3.11 where stdlib tomllib is absent."""
    root = {}
    cur = root

    def _parse_val(raw: str):
        v = raw.strip()
        if v == "true":
            return True
        if v == "false":
            return False
        if v.startswith('"') and v.endswith('"'):
            return json.loads(v)
        if v.startswith("'") and v.endswith("'"):
            return v[1:-1]
        if re.fullmatch(r"-?\d+", v):
            return int(v)
        if re.fullmatch(r"-?\d+\.\d+", v):
            return float(v)
        return v

    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[[") and line.endswith("]]"):
            key = line[2:-2].strip()
            arr = root.setdefault(key, [])
            item = {}
            arr.append(item)
            cur = item
        elif line.startswith("[") and line.endswith("]"):
            parts = [p.strip() for p in line[1:-1].split(".")]
            target = root
            for p in parts:
                nxt = target.setdefault(p, {})
                if isinstance(nxt, list):
                    nxt = nxt[-1]
                target = nxt
            cur = target
        elif "=" in line:
            k, v = line.split("=", 1)
            cur[k.strip()] = _parse_val(v)
    return root


class TestPlaytester1199AcceptanceCriteria(unittest.TestCase):
    def test_cli_parses_hybrid_continuous_and_no_hybrid(self):
        args_hc = playtester.parse_args(["--hybrid", "--continuous"])
        self.assertTrue(args_hc.hybrid)
        self.assertTrue(args_hc.continuous)

        args_nh = playtester.parse_args(["--no-hybrid"])
        self.assertFalse(args_nh.hybrid)
        self.assertFalse(args_nh.continuous)

        args_default = playtester.parse_args([])
        self.assertTrue(args_default.hybrid)

    def test_write_server_config_uses_only_valid_rust_config_rs_keys(self):
        config_rs_path = playtester.REPO_ROOT / "crates" / "eve-server" / "src" / "config.rs"
        config_src = config_rs_path.read_text(encoding="utf-8")

        struct_fields = {}
        for m in re.finditer(r"pub struct (\w+)\s*\{(.*?)\n\}", config_src, flags=re.DOTALL):
            sname, body = m.group(1), m.group(2)
            struct_fields[sname] = set(re.findall(r"^\s*pub\s+([a-zA-Z0-9_]+)\s*:", body, flags=re.MULTILINE))

        for required in (
            "ServerConfig",
            "FeatureFlagsConfig",
            "PlayerAccountConfig",
            "StarterCharacterConfig",
            "AdminAccountConfig",
        ):
            self.assertIn(required, struct_fields)

        with tempfile.TemporaryDirectory() as td:
            wt = Path(td)
            args = playtester.parse_args(["--worktree", str(wt), "--no-hybrid"])
            runner = playtester.PlaytesterRunner(args)
            cfg_path = runner.write_server_config()
            cfg_text = cfg_path.read_text(encoding="utf-8")
            parsed = tomllib.loads(cfg_text) if tomllib is not None else _parse_simple_toml(cfg_text)

        unknown_top = set(parsed.keys()) - struct_fields["ServerConfig"]
        self.assertEqual(unknown_top, set(), f"Unknown ServerConfig keys: {unknown_top}")

        ff = parsed.get("feature_flags", {})
        unknown_ff = set(ff.keys()) - struct_fields["FeatureFlagsConfig"]
        self.assertEqual(unknown_ff, set(), f"Unknown FeatureFlagsConfig keys: {unknown_ff}")
        self.assertTrue(ff.get("enable_quasar_gateway"))
        self.assertTrue(ff.get("enable_esi_http"))
        self.assertTrue(ff.get("enable_living_world"))
        self.assertTrue(ff.get("seed_demo"))
        self.assertTrue(ff.get("free_fast_checkout_plex"))
        self.assertTrue(ff.get("gm_mode"))
        self.assertGreater(int(ff.get("starter_plex_balance", 0)), 0)

        players = parsed.get("player_accounts", [])
        self.assertGreaterEqual(len(players), 1)
        for p_acc in players:
            unknown_p = set(p_acc.keys()) - struct_fields["PlayerAccountConfig"]
            self.assertEqual(unknown_p, set(), f"Unknown PlayerAccountConfig keys: {unknown_p}")
            sc = p_acc.get("starter_character")
            self.assertIsInstance(sc, dict)
            unknown_sc = set(sc.keys()) - struct_fields["StarterCharacterConfig"]
            self.assertEqual(unknown_sc, set(), f"Unknown StarterCharacterConfig keys: {unknown_sc}")
            self.assertGreater(int(sc.get("initial_balance_isk", 0)), 0)

        admins = parsed.get("admin_accounts", [])
        self.assertEqual(len(admins), 0, f"Default player persona must run role=0 with no admin_accounts, got {admins}")

        with tempfile.TemporaryDirectory() as td_gm:
            wt_gm = Path(td_gm)
            args_gm = playtester.parse_args(["--worktree", str(wt_gm), "--no-hybrid", "--persona", "gm_tester"])
            runner_gm = playtester.PlaytesterRunner(args_gm)
            cfg_gm_text = runner_gm.write_server_config().read_text(encoding="utf-8")
            parsed_gm = tomllib.loads(cfg_gm_text) if tomllib is not None else _parse_simple_toml(cfg_gm_text)
        # BF-1242-R1-03 & #1539: neither default nor --persona gm_tester emits [[admin_accounts]] (which would
        # clobber seed_demo); both set EVOMU_ADMIN_TOKEN (`admin` 90000003) via `_server_extra_env()` so
        # `demo` stays role=0 while host GM/timeControl bridge ops run out-of-band.
        admins_gm = parsed_gm.get("admin_accounts", [])
        self.assertEqual(len(admins_gm), 0, f"gm_tester must not emit admin_accounts, got {admins_gm}")
        self.assertTrue(parsed_gm.get("feature_flags", {}).get("seed_demo"))
        self.assertEqual(
            runner._server_extra_env(),
            {"EVOMU_ADMIN_TOKEN": playtester.PLAYTESTER_GM_ADMIN_SECRET},
        )
        self.assertEqual(
            runner_gm._server_extra_env(),
            {"EVOMU_ADMIN_TOKEN": playtester.PLAYTESTER_GM_ADMIN_SECRET},
        )

    def test_start_ini_build_detection_and_staging_without_hardcoded_build(self):
        src_text = (HARNESS_DIR / "playtester.py").read_text(encoding="utf-8")
        self.assertNotIn("3624425", src_text)

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            fake_home = root / "home"
            fake_build = root / "eve_shared" / "build"
            fake_cache = root / "cache"
            (fake_build / "bin64").mkdir(parents=True)
            (fake_build / "start.ini").write_text("[main]\nbuild = 1234567\n", encoding="utf-8")
            (fake_build / "bin64" / "exefile").write_bytes(b"fake-exefile")
            inclient_cfg = root / "inclient_cfg.json"
            inclient_cfg.write_text(json.dumps({"window_width": 1280, "window_height": 800}), encoding="utf-8")

            with mock.patch.dict(os.environ, {"HOME": str(fake_home)}), \
                 mock.patch.object(playtester, "SYSTEM_EVE_BUILD_DIR", fake_build), \
                 mock.patch.object(playtester, "DEFAULT_CACHE_ROOT", fake_cache), \
                 mock.patch.object(playtester, "SOURCE_STAGED_APP", None):
                detected = playtester._detect_system_eve_build(fake_build, cache_root=fake_cache)
                self.assertEqual(detected, "1234567")

                expected_stage = fake_cache / "client_build_1234567_playtester"
                self.assertEqual(playtester.resolve_default_staged_app(), expected_stage)
                self.assertEqual(Path(playtester.parse_args([]).staged_app), expected_stage)

                stage_dir = playtester.stage_playtester_client(None, inclient_cfg, 26099)
                self.assertEqual(stage_dir, expected_stage)
                self.assertTrue((stage_dir / "bin64" / "exefile").exists())
                self.assertTrue((stage_dir / "evomu_py_site" / "sitecustomize.py").exists())
                self.assertTrue((stage_dir / "tools" / "startup" / "evomu_playtester.py").exists())


class TestPlaytester1200OriginMainCutover(unittest.TestCase):
    @staticmethod
    def _git(repo: Path, *args: str) -> str:
        res = subprocess.run(
            ["git", "-C", str(repo), *args],
            check=True,
            capture_output=True,
            text=True,
        )
        return res.stdout

    def _init_fake_origin_and_worktree(self, root: Path):
        origin = root / "origin.git"
        subprocess.run(["git", "init", "--bare", "-b", "main", str(origin)], check=True, capture_output=True)
        seed = root / "seed"
        subprocess.run(["git", "clone", str(origin), str(seed)], check=True, capture_output=True)
        self._git(seed, "config", "user.email", "test@evomu.local")
        self._git(seed, "config", "user.name", "EvoMu Test")

        (seed / ".gitignore").write_text(".playtester/\n.evomu-bugs/\n", encoding="utf-8")
        (seed / "AGENTS.md").write_text("rules v1\n", encoding="utf-8")
        (seed / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
        (seed / "crates" / "eve-server" / "src").mkdir(parents=True)
        (seed / "crates" / "eve-server" / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
        (seed / "tools" / "client-harness").mkdir(parents=True)
        (seed / "tools" / "client-harness" / "playtester.py").write_text("# v1\n", encoding="utf-8")
        (seed / "docs").mkdir()
        (seed / "docs" / "obsolete.md").write_text("obsolete\n", encoding="utf-8")
        self._git(seed, "add", ".")
        self._git(seed, "commit", "-m", "c1")
        self._git(seed, "push", "origin", "main")
        c1 = self._git(seed, "rev-parse", "HEAD").strip()

        wt = root / "wt"
        subprocess.run(["git", "clone", str(origin), str(wt)], check=True, capture_output=True)
        self._git(wt, "config", "user.email", "test@evomu.local")
        self._git(wt, "config", "user.name", "EvoMu Test")
        return origin, seed, wt, c1

    def test_check_origin_main_update_cuts_over_all_tracked_files_and_preserves_runtime_state(self):
        with tempfile.TemporaryDirectory() as td:
            _, seed, wt, c1 = self._init_fake_origin_and_worktree(Path(td))

            # Simulate stale tracked modifications outside crates/ + staged runtime file in .playtester/
            (wt / "AGENTS.md").write_text("stale local AGENTS\n", encoding="utf-8")
            (wt / "tools" / "client-harness" / "playtester.py").write_text("# stale harness\n", encoding="utf-8")
            (wt / ".playtester").mkdir()
            pending_file = wt / ".playtester" / "pending_verifications.json"
            memory_file = wt / ".playtester" / "memory.json"
            pending_file.write_text('{"items":[{"issue":1200}]}', encoding="utf-8")
            memory_file.write_text('{"total_steps":42}', encoding="utf-8")
            self._git(wt, "add", "-f", ".playtester/pending_verifications.json")

            # Advance fake origin/main across crates/, tools/, AGENTS.md, .agents/, and delete docs/obsolete.md
            (seed / "crates" / "eve-server" / "src" / "main.rs").write_text("fn main() { println!(\"v2\"); }\n", encoding="utf-8")
            (seed / "AGENTS.md").write_text("rules v2\n", encoding="utf-8")
            (seed / "tools" / "client-harness" / "playtester.py").write_text("# v2\n", encoding="utf-8")
            (seed / "tools" / "client-harness" / "curate_memory.py").write_text("# new module\n", encoding="utf-8")
            (seed / ".agents").mkdir()
            (seed / ".agents" / "review.py").write_text("# review gate v2\n", encoding="utf-8")
            (seed / "docs" / "obsolete.md").unlink()
            self._git(seed, "add", "-A")
            self._git(seed, "commit", "-m", "c2")
            self._git(seed, "push", "origin", "main")
            c2 = self._git(seed, "rev-parse", "HEAD").strip()

            self.assertEqual(self._git(wt, "rev-parse", "HEAD").strip(), c1)
            new_sha = playtester.check_origin_main_update(wt)
            self.assertEqual(new_sha, c2[:9])

            # No tracked file differs from origin/main after cut-over
            self.assertEqual(self._git(wt, "diff", "origin/main", "--stat"), "")
            self.assertEqual(self._git(wt, "status", "--porcelain"), "")
            self.assertEqual((wt / "AGENTS.md").read_text(encoding="utf-8"), "rules v2\n")
            self.assertEqual((wt / "tools" / "client-harness" / "playtester.py").read_text(encoding="utf-8"), "# v2\n")
            self.assertEqual((wt / "tools" / "client-harness" / "curate_memory.py").read_text(encoding="utf-8"), "# new module\n")
            self.assertEqual((wt / ".agents" / "review.py").read_text(encoding="utf-8"), "# review gate v2\n")
            self.assertFalse((wt / "docs" / "obsolete.md").exists())

            # Runtime .playtester/ state is preserved and untracked/ignored
            self.assertEqual(pending_file.read_text(encoding="utf-8"), '{"items":[{"issue":1200}]}')
            self.assertEqual(memory_file.read_text(encoding="utf-8"), '{"total_steps":42}')

            # Second call when already synced is a clean no-op
            self.assertIsNone(playtester.check_origin_main_update(wt))

            # Strictly-ahead local branch is not reset backwards
            (wt / "tools" / "client-harness" / "local_ahead.py").write_text("# ahead\n", encoding="utf-8")
            self._git(wt, "add", "tools/client-harness/local_ahead.py")
            self._git(wt, "commit", "-m", "local ahead")
            self.assertIsNone(playtester.check_origin_main_update(wt))
            self.assertTrue((wt / "tools" / "client-harness" / "local_ahead.py").exists())

    def test_hot_reload_server_boundary_heals_prior_reset_soft_index_drift(self):
        with tempfile.TemporaryDirectory() as td:
            _, seed, wt, _ = self._init_fake_origin_and_worktree(Path(td))
            (seed / "AGENTS.md").write_text("rules v2\n", encoding="utf-8")
            (seed / "tools" / "client-harness" / "playtester.py").write_text("# v2\n", encoding="utf-8")
            self._git(seed, "add", "-A")
            self._git(seed, "commit", "-m", "c2")
            self._git(seed, "push", "origin", "main")

            # Simulate legacy `reset --soft` drift where HEAD == origin/main but files outside crates/ stayed at v1
            self._git(wt, "fetch", "origin", "main")
            self._git(wt, "reset", "--soft", "origin/main")
            self.assertNotEqual(self._git(wt, "diff", "origin/main", "--stat"), "")

            runner = playtester.PlaytesterRunner.__new__(playtester.PlaytesterRunner)
            runner.args = types.SimpleNamespace(no_git_checkout=False)
            runner.worktree_dir = wt
            runner.current_run_dir = wt / ".playtester" / "run"
            runner.current_run_dir.mkdir(parents=True)
            runner.reload_requested = True
            runner._server_bin_mtime = lambda: 123.0
            runner.stop_client = lambda: None
            runner.stop_server = lambda: None
            runner.start_server = lambda reset_db=False: True
            runner.start_client = lambda: True
            runner.reset_batch_state = lambda: None
            runner.oracle = types.SimpleNamespace(reset_log_offsets=lambda *a: None)
            runner.memory = types.SimpleNamespace(
                data={"server_reloads": 0, "long_term_notes": []},
                normalize_coverage_frontier=lambda **kw: {"buggy_decayed": 0},
                save=lambda: None,
            )

            with mock.patch.object(playtester, "compile_eve_server", return_value=(True, wt / "eve-server", "")), \
                 mock.patch.object(playtester, "DEFAULT_CONTROL_FILE", Path(td) / "missing.control"):
                ok = runner.hot_reload_server_boundary()

            self.assertTrue(ok)
            self.assertEqual(self._git(wt, "diff", "origin/main", "--stat"), "")
            self.assertEqual(self._git(wt, "status", "--porcelain"), "")
            self.assertEqual((wt / "AGENTS.md").read_text(encoding="utf-8"), "rules v2\n")
            self.assertEqual((wt / "tools" / "client-harness" / "playtester.py").read_text(encoding="utf-8"), "# v2\n")

    def test_hot_reload_server_boundary_preserves_state_when_compile_fails(self):
        with tempfile.TemporaryDirectory() as td:
            wt = Path(td)
            runner = playtester.PlaytesterRunner.__new__(playtester.PlaytesterRunner)
            runner.args = types.SimpleNamespace(no_git_checkout=True)
            runner.worktree_dir = wt
            runner.current_run_dir = wt / ".playtester" / "run"
            runner.current_run_dir.mkdir(parents=True)
            runner.reload_requested = True
            runner.cached_git_head = "abc123456"

            lifecycle_calls = []
            runner.stop_client = lambda: lifecycle_calls.append("stop_client")
            runner.stop_server = lambda: lifecycle_calls.append("stop_server")
            runner.start_server = lambda reset_db=False: lifecycle_calls.append("start_server") or True
            runner.start_client = lambda: lifecycle_calls.append("start_client") or True
            runner.reset_batch_state = lambda: lifecycle_calls.append("reset_batch_state")
            save_calls = []
            runner.memory = types.SimpleNamespace(
                data={"server_reloads": 2, "long_term_notes": ["prior"]},
                normalize_coverage_frontier=lambda **_kw: {"buggy_decayed": 0},
                save=lambda: save_calls.append(True),
            )

            with mock.patch.object(playtester, "compile_eve_server", return_value=(False, None, "cargo compile error")), \
                 mock.patch.object(playtester, "DEFAULT_CONTROL_FILE", wt / "missing.control"):
                ok = runner.hot_reload_server_boundary()

            self.assertFalse(ok)
            self.assertEqual(lifecycle_calls, [])
            self.assertEqual(runner.cached_git_head, "abc123456")
            self.assertEqual(runner.memory.data["server_reloads"], 2)
            self.assertEqual(runner.memory.data["long_term_notes"], ["prior"])
            self.assertEqual(save_calls, [])

    def test_invalid_target_benign_user_error_and_tasklet_classification(self):
        self.assertIn("invalidtarget", playtester.PlaytesterBugOracle.EXPECTED_USER_ERROR_KEYWORDS)
        self.assertIn("invalidtarget", playtester.PlaytesterBugOracle.BENIGN_USER_ERROR_MARKERS)
        oracle = playtester.PlaytesterBugOracle
        self.assertTrue(
            oracle.is_expected_user_error(
                {"error": "UserError('CustomInfo', {'info': 'InvalidTarget: Target is not valid'})"},
                expected_text="Activate turret",
            )
        )
        tb_invalid_target = (
            "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x9, ctxt='ModuleButton.OnStateChange'>\",)\n"
            '  File "eve/client/script/ui/inflight/shipModuleButton/shipmodulebutton.py", line 1630, in OnStateChange\n'
            '  File "eve/client/script/ui/inflight/shipModuleButton/shipmodulebutton.py", line 1580, in ActivateEffect\n'
            "UserError: User error, msg=CustomInfo, dict={'info': 'InvalidTarget'}\n"
        )
        self.assertFalse(oracle._is_unhandled_tasklet_traceback("Unhandled exception in ModuleButton.OnStateChange", tb_invalid_target))
        self.assertTrue(oracle.is_ignorable_client_traceback("Unhandled exception in ModuleButton.OnStateChange", tb_invalid_target))

    def test_flight_recorder_anomaly_harvester_high_err_and_polling_storm(self):
        with tempfile.TemporaryDirectory() as td:
            wt = Path(td)
            args = playtester.parse_args(["--worktree", str(wt)])
            runner = playtester.PlaytesterRunner(args)
            runner.oracle.async_write = False
            runner.client_proc = None

            base_ts = 1700000000.0
            # 1) Benign allowlisted errors (DockRequested / SessionChangeInProgress / InvalidTarget) -> 0 bundles
            benign_calls = [
                {"seq": 1, "ts": base_ts + 1, "service": "station", "method": "Dock", "status": "ERR", "error": "UserError('DockRequested')"},
                {"seq": 2, "ts": base_ts + 2, "service": "station", "method": "Dock", "status": "ERR", "error": "UserError('DockRequested')"},
                {"seq": 3, "ts": base_ts + 3, "service": "station", "method": "Dock", "status": "ERR", "error": "UserError('DockRequested')"},
                {"seq": 4, "ts": base_ts + 4, "service": "sessionMgr", "method": "Undock", "status": "ERR", "error": "UserError('SessionChangeInProgress')"},
                {"seq": 5, "ts": base_ts + 5, "service": "sessionMgr", "method": "Undock", "status": "ERR", "error": "UserError('SessionChangeInProgress')"},
                {"seq": 6, "ts": base_ts + 6, "service": "sessionMgr", "method": "Undock", "status": "ERR", "error": "UserError('SessionChangeInProgress')"},
            ]
            emitted_benign = runner.harvest_flight_recorder_anomalies(step_num=10, calls_override=benign_calls)
            self.assertEqual(emitted_benign, [])

            # 2) 5/5 unexpected failing RPCs -> emits 1 deduplicated anomaly bundle; second call emits 0
            err_calls = [
                {"seq": 100 + i, "ts": base_ts + i, "service": "skillPlanSvc", "method": "GetAllPersonal", "status": "ERR", "error": "InternalError('db timeout')"}
                for i in range(5)
            ]
            emitted_err_1 = runner.harvest_flight_recorder_anomalies(step_num=11, calls_override=err_calls)
            self.assertEqual(len(emitted_err_1), 1)
            bundle_err = json.loads(emitted_err_1[0].read_text(encoding="utf-8"))
            self.assertEqual(bundle_err["report_source"], "flight_recorder_harvester")
            self.assertEqual(bundle_err["anomaly"]["service"], "skillPlanSvc")
            self.assertEqual(bundle_err["anomaly"]["non_ok_calls"], 5)
            self.assertEqual(runner.harvest_flight_recorder_anomalies(step_num=12, calls_override=err_calls), [])

            # 3) 300/min polling loop (300 OK calls inside 60s) -> emits 1 deduplicated polling-storm bundle
            storm_calls = [
                {"seq": 1000 + i, "ts": base_ts + (i * 0.19), "service": "marketProxy", "method": "GetRegionBest", "status": "OK", "error": ""}
                for i in range(300)
            ]
            emitted_storm = runner.harvest_flight_recorder_anomalies(step_num=13, calls_override=storm_calls)
            self.assertEqual(len(emitted_storm), 1)
            bundle_storm = json.loads(emitted_storm[0].read_text(encoding="utf-8"))
            self.assertEqual(bundle_storm["kind"], "flight_recorder_polling_storm")
            self.assertEqual(bundle_storm["anomaly"]["service"], "marketProxy")
            self.assertGreater(bundle_storm["anomaly"]["peak_per_min"], 200.0)
            self.assertEqual(runner.harvest_flight_recorder_anomalies(step_num=14, calls_override=storm_calls), [])

    def test_exploratory_validator_rejects_non_gm_slash_at_step_gt_zero(self):
        # Unit check on validate_exploratory_action
        slash_create = {"type": "slash_command", "command": "/createitem 34 100"}
        ok0, _ = playtester.validate_exploratory_action(slash_create, step_num=0, persona_id="open_explorer")
        self.assertTrue(ok0)
        ok_gm, _ = playtester.validate_exploratory_action(slash_create, step_num=3, persona_id="gm_tester")
        self.assertTrue(ok_gm)
        ok_player, reason_player = playtester.validate_exploratory_action(slash_create, step_num=1, persona_id="open_explorer")
        self.assertFalse(ok_player)
        self.assertIn("Setup-only GM enforcement rejected", reason_player)

        ok_text_slash, _ = playtester.validate_exploratory_action(
            {"type": "type_text", "text": "/createitem 34 50"}, step_num=2, persona_id="market_trader"
        )
        self.assertFalse(ok_text_slash)

        # End-to-end execute_step check: execute_action RPC must NOT be dispatched for /createitem at step > 0
        with tempfile.TemporaryDirectory() as td:
            wt = Path(td)
            args = playtester.parse_args(["--worktree", str(wt)])
            runner = playtester.PlaytesterRunner(args)
            runner.oracle.async_write = False
            runner.cached_git_head = "deadbeef"
            snap = {
                "active_view": "hangar",
                "session": {"charid": 90000001, "stationid": 60003760},
                "ui": {"open_windows": [], "modals": [], "clickable_widgets": []},
                "open_windows": [],
                "modals": [],
                "space": {},
                "skills_state": {},
                "wallet_state": {},
            }
            rpc_calls = []

            def fake_rpc_call(method, params=None, timeout=15.0):
                rpc_calls.append(method)
                if method == "get_state":
                    return dict(snap)
                return {"ok": True}

            runner.rpc = mock.Mock()
            runner.rpc.call.side_effect = fake_rpc_call
            plan_dec = {
                "observation": "In hangar",
                "previous_action_verification": {"expectation_met": True, "discrepancy_summary": ""},
                "bug_report": {"should_file": False},
                "current_goal": "Spawn item via /createitem",
                "memory_update": {},
                "delegated_plan": {
                    "plan_goal": "Try GM slash in player persona",
                    "horizon_rationale": "Test rejection",
                    "max_batch_steps": 1,
                    "steps": [
                        {
                            "step_number": 1,
                            "exec_mode": "direct",
                            "action": {"type": "slash_command", "command": "/createitem 34 100"},
                        }
                    ],
                },
            }
            with mock.patch.object(playtester, "capture_window_screenshot", return_value={"ok": False}), \
                 mock.patch.object(playtester, "invoke_agy_planner", return_value=(plan_dec, "hydrogen-l-tf-medium", 0.5)):
                runner.execute_step(1, {}, [])
            self.assertNotIn("execute_action", rpc_calls)

    def test_client_traceback_v24_build_path_stripped_no_fingerprint_collision_1141(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            qdir = root / "queue"
            sdir = root / "step_50"
            qdir.mkdir()
            sdir.mkdir()
            shot = root / "shot.png"
            shot.write_bytes(b"png")
            tb = root / "tb.log"
            tb.write_text("", encoding="utf-8")
            srv = root / "srv.log"
            srv.write_text("", encoding="utf-8")
            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            build_root = "/" + "Users" + "/buildagentA/buildAgentFull"
            exc_a = (
                "STACKTRACE: Traceback (most recent call last):\n"
                f'  File "{build_root}/work/d50fcc72fcb6ee81/eve/release/V24.02/'
                'eve/client/script/ui/shared/market/marketWindow.py", line 412, in LoadMarketOrders\n'
                f'  File "{build_root}/work/d50fcc72fcb6ee81/eve/release/V24.02/'
                'packages/carbonui/control/scroll.py", line 188, in Reload\n'
                "KeyError: 'marketGroupID'"
            )
            exc_b = (
                "STACKTRACE: Traceback (most recent call last):\n"
                f'  File "{build_root}/work/deadbeefcafebabe/eve/release/V24.02/'
                'eve/client/script/ui/shared/agencyNew/ui/contentCards.py", line 91, in ConstructCard\n'
                "AttributeError: 'NoneType' object has no attribute 'agencyContentType'"
            )
            self.assertNotEqual(oracle._tb_frame_sig(exc_a), oracle._tb_frame_sig(exc_b))
            self.assertIn("scroll.py", oracle._tb_frame_sig(exc_a))
            self.assertIn("KeyError", oracle._tb_frame_sig(exc_a))
            self.assertNotIn(build_root, oracle._extract_tb_headline(exc_a))
            self.assertIn("KeyError", oracle._extract_tb_headline(exc_a))

            # Frame-only V24.02 lines (previous root cause of colliding on .../eve/clien)
            frame_only_a = (
                f'File "{build_root}/work/d50fcc72fcb6ee81/eve/release/V24.02/'
                'eve/client/script/ui/shared/market/marketWindow.py", line 412, in LoadMarketOrders'
            )
            frame_only_b = (
                f'File "{build_root}/work/d50fcc72fcb6ee81/eve/release/V24.02/'
                'eve/client/script/ui/shared/agencyNew/ui/contentCards.py", line 91, in ConstructCard'
            )
            self.assertIn("marketWindow.py", oracle._extract_tb_headline(frame_only_a))
            self.assertNotIn(build_root, oracle._extract_tb_headline(frame_only_a))
            self.assertNotEqual(oracle._extract_tb_headline(frame_only_a), oracle._extract_tb_headline(frame_only_b))

            fr_a = {
                "log_exceptions": [exc_a],
                "suppressed_log_exceptions": [],
                "macho_exceptions": [],
                "quasar_errors": [],
            }
            fr_b = {
                "log_exceptions": [exc_b],
                "suppressed_log_exceptions": [],
                "macho_exceptions": [],
                "quasar_errors": [],
            }
            paths_a = oracle.evaluate_step(
                step_num=50,
                git_head="deadbeef",
                decision={"action": {"type": "click_widget", "target": "MarketBtn"}, "bug_report": {"should_file": False}},
                exec_res={"action_ok": True, "flight_recorder": fr_a},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder=fr_a,
            )
            paths_b = oracle.evaluate_step(
                step_num=51,
                git_head="deadbeef",
                decision={"action": {"type": "click_widget", "target": "AgencyBtn"}, "bug_report": {"should_file": False}},
                exec_res={"action_ok": True, "flight_recorder": fr_b},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder=fr_b,
            )
            self.assertEqual(len(paths_a), 1)
            self.assertEqual(len(paths_b), 1)
            bundle_a = json.loads(Path(paths_a[0]).read_text(encoding="utf-8"))
            bundle_b = json.loads(Path(paths_b[0]).read_text(encoding="utf-8"))
            self.assertNotEqual(bundle_a["fingerprint"], bundle_b["fingerprint"])
            self.assertNotEqual(bundle_a["fingerprint"], "997f1f2ca1a786b9")
            self.assertNotEqual(bundle_b["fingerprint"], "997f1f2ca1a786b9")
            self.assertNotIn(build_root, bundle_a["manual_report"]["title"])
            self.assertNotIn(build_root, bundle_b["manual_report"]["title"])
            self.assertIn("scroll.py", bundle_a["manual_report"]["description"])
            self.assertIn("contentCards.py", bundle_b["manual_report"]["description"])

    def test_gm_tester_preserves_demo_seed_and_routes_gm_ops_via_admin_bridge(self):
        import types

        with tempfile.TemporaryDirectory() as td:
            wt = Path(td)
            args = playtester.parse_args(["--worktree", str(wt), "--persona", "gm_tester"])
            runner = playtester.PlaytesterRunner(args)
            runner.oracle.async_write = False
            runner.cached_git_head = "deadbeef"
            cfg_text = runner.write_server_config().read_text(encoding="utf-8")
            self.assertNotIn("[[admin_accounts]]", cfg_text)
            self.assertIn("seed_demo = true", cfg_text)
            self.assertEqual(
                runner._server_extra_env(),
                {"EVOMU_ADMIN_TOKEN": playtester.PLAYTESTER_GM_ADMIN_SECRET},
            )

            snap = {
                "active_view": "hangar",
                "session": {"charid": 90000001, "stationid": 60003760},
                "ui": {"open_windows": [], "modals": [], "clickable_widgets": []},
                "open_windows": [],
                "modals": [],
                "space": {},
                "skills_state": {},
                "wallet_state": {},
            }
            rpc_calls = []
            gm_events = []

            class _FakeGmConn:
                def __init__(self):
                    self.connected = True
                def close(self):
                    self.connected = False
                    gm_events.append(("close",))

            class _FakeSvc:
                def Login(self, u, pw):
                    gm_events.append(("Login", u, pw))
                    return True
                def GetCharactersToSelect(self):
                    return [{"characterID": 90000003}]
                def SelectCharacterID(self, cid):
                    gm_events.append(("SelectCharacterID", cid))
                    return True
                def SlashCmd(self, cmd):
                    gm_events.append(("SlashCmd", cmd))
                    return {"ok": True}

            fake_bridge = types.ModuleType("evomu_bridge")
            def _install_hook(sm, host="127.0.0.1", port=26000):
                sm.RemoteSvc = lambda _name: _FakeSvc()
                return _FakeGmConn()
            fake_bridge.install_service_manager_hook = _install_hook

            def fake_rpc_call(method, params=None, timeout=15.0):
                rpc_calls.append(method)
                if method == "get_state":
                    return dict(snap)
                return {"ok": True}

            runner.rpc = mock.Mock()
            runner.rpc.call.side_effect = fake_rpc_call
            plan_dec = {
                "observation": "In hangar",
                "previous_action_verification": {"expectation_met": True, "discrepancy_summary": ""},
                "bug_report": {"should_file": False},
                "current_goal": "Spawn item via /createitem in gm_tester",
                "memory_update": {},
                "delegated_plan": {
                    "plan_goal": "GM bridge /createitem",
                    "horizon_rationale": "Test gm_tester admin bridge",
                    "max_batch_steps": 1,
                    "steps": [
                        {
                            "step_number": 1,
                            "exec_mode": "direct",
                            "action": {"type": "slash_command", "command": "/createitem 34 100"},
                        }
                    ],
                },
            }
            with mock.patch.dict(sys.modules, {"evomu_bridge": fake_bridge}), \
                 mock.patch.object(playtester, "capture_window_screenshot", return_value={"ok": False}), \
                 mock.patch.object(playtester, "invoke_agy_planner", return_value=(plan_dec, "hydrogen-l-tf-medium", 0.5)):
                runner.execute_step(1, {}, [])
            runner._close_gm_bridge()

            self.assertNotIn("execute_action", rpc_calls)
            self.assertIn(("Login", "admin", playtester.PLAYTESTER_GM_ADMIN_SECRET), gm_events)
            self.assertIn(("SelectCharacterID", 90000003), gm_events)
            self.assertIn(("SlashCmd", "/createitem 90000001 34 100"), gm_events)
            self.assertIn(("close",), gm_events)

    def test_planner_and_executor_prompts_omit_gm_actions_past_step_zero_for_non_gm(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            mem.data["active_persona"] = {"id": "open_explorer", "title": "Explorer", "rotate_after_steps": 25}
            pp_player_s1 = playtester.build_planner_prompt(
                1, root / "screen.png", [], {"active_view": "hangar"}, {}, mem, "deadbeef"
            )
            ep_player_s1 = playtester.build_executor_prompt(
                1, 1, 1, {"plan_goal": "Test", "max_batch_steps": 1, "steps": [{}]},
                root / "screen.png", [], {"active_view": "hangar"}, {}, mem, [],
            )
            self.assertNotIn("train_skill", pp_player_s1)
            self.assertNotIn("open_structure_cosmetics", pp_player_s1)
            self.assertNotIn("train_skill", ep_player_s1)
            self.assertNotIn("open_structure_cosmetics", ep_player_s1)

            pp_player_s0 = playtester.build_planner_prompt(
                0, root / "screen.png", [], {"active_view": "hangar"}, {}, mem, "deadbeef"
            )
            self.assertIn("train_skill", pp_player_s0)
            self.assertIn("open_structure_cosmetics", pp_player_s0)

            mem.data["active_persona"] = dict(playtester.GM_TESTER_PERSONA)
            pp_gm_s2 = playtester.build_planner_prompt(
                2, root / "screen.png", [], {"active_view": "hangar"}, {}, mem, "deadbeef"
            )
            self.assertIn("train_skill", pp_gm_s2)
            self.assertIn("open_structure_cosmetics", pp_gm_s2)

    def test_loaded_non_gm_personas_and_stale_memory_omit_open_structure_cosmetics(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            state_dir = root / ".playtester"
            state_dir.mkdir(parents=True, exist_ok=True)
            # Simulate pre-existing memory.json carrying the stale open_structure_cosmetics acceptance.
            (state_dir / "memory.json").write_text(
                json.dumps(
                    {
                        "active_persona": {"id": "regression_and_critical_ui"},
                        "coverage_frontier": [
                            {
                                "feature": "quasar.cosmetics.structure_states",
                                "persona": "regression_and_critical_ui",
                                "status": "untested",
                                "acceptance": "Invoke open_structure_cosmetics cleanly via publicGatewaySvc without TimeoutException.",
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            for persona_id in ("regression_and_critical_ui", "social_corp_fleet_and_sovereignty"):
                mem.maybe_rotate_persona(force_id=persona_id)
                pp = playtester.build_planner_prompt(
                    1, root / "screen.png", [], {"active_view": "hangar"}, {}, mem, "deadbeef"
                )
                ep = playtester.build_executor_prompt(
                    1, 1, 1, {"plan_goal": "Test", "max_batch_steps": 1, "steps": [{}]},
                    root / "screen.png", [], {"active_view": "hangar"}, {}, mem, [],
                )
                self.assertNotIn("open_structure_cosmetics", pp)
                self.assertNotIn("open_structure_cosmetics", ep)


class TestBenignClientLogExceptionsFiltering1313(unittest.TestCase):
    _MINIMAP_TRACE = (
        "log.LogException args=() kwargs={}\n"
        "Traceback (most recent call last):\n"
        '  File "packages/carbonui/uilib.py", line 695, in Update\n'
        '  File "packages/carbonui/graphs/minimap.py", line 100, in UpdateAlignment\n'
        '  File "packages/carbonui/graphs/minimap.py", line 111, in _ResizeSlider\n'
        "AttributeError: 'MapSlider' object has no attribute 'minimumWidth'\n"
    )
    _MARKETBASE_RESIZE_TRACE = (
        "log.LogException args=() kwargs={}\n"
        "Traceback (most recent call last):\n"
        '  File "packages/carbonui/uilib.py", line 695, in Update\n'
        '  File "eve/client/script/ui/shared/market/marketbase.py", line 3718, in _OnResize\n'
        "AttributeError: 'NoneType' object has no attribute 'max'\n"
    )
    _REAL_TRACE = (
        "log.LogException args=() kwargs={}\n"
        "Traceback (most recent call last):\n"
        '  File "eve/client/script/ui/shared/market/marketWindow.py", line 412, in LoadMarketOrders\n'
        "KeyError: 'orders'\n"
    )
    _UNHANDLED_TASKLET_TRACE = (
        "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x12, ctxt='market.LoadData'>\",)\n"
        "Traceback (most recent call last):\n"
        '  File "eve/client/script/ui/shared/market/marketWindow.py", line 210, in LoadData\n'
        "UserError: ('CustomInfo', {'info': 'UnexpectedServerState'})\n"
    )

    def test_prompts_separate_benign_minimap_and_marketbase_log_exceptions_without_mutating_input(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            shot = root / "screen.png"
            shot.write_bytes(b"png")

            minimap_item = {
                "headline": "AttributeError: 'MapSlider' object has no attribute 'minimumWidth'",
                "trace": self._MINIMAP_TRACE,
            }
            marketbase_item = {
                "headline": "AttributeError: 'NoneType' object has no attribute 'max'",
                "trace": self._MARKETBASE_RESIZE_TRACE,
            }
            real_item = {
                "headline": "KeyError: 'orders'",
                "trace": self._REAL_TRACE,
            }

            raw_benign_fr = {
                "log_exceptions": [minimap_item, marketbase_item],
                "quasar_errors": [],
                "macho_exceptions": [],
            }
            r_prompt = playtester.build_reasoning_prompt(
                step_num=57954,
                screenshot_path=shot,
                burst_paths=[],
                ui_snapshot={"active_view": "hangar"},
                last_flight_recorder=raw_benign_fr,
                memory=mem,
                git_head="deadbeef",
            )
            self.assertEqual(len(raw_benign_fr["log_exceptions"]), 2)
            self.assertIn('"benign_log_exceptions":', r_prompt)
            self.assertIn('"log_exceptions":[]', r_prompt.replace(" ", ""))

            p_prompt = playtester.build_planner_prompt(
                step_num=53111,
                screenshot_path=shot,
                burst_paths=[],
                ui_snapshot={"active_view": "inflight"},
                last_flight_recorder=raw_benign_fr,
                memory=mem,
                git_head="deadbeef",
            )
            self.assertIn('"benign_log_exceptions":', p_prompt)
            self.assertIn('"log_exceptions":[]', p_prompt.replace(" ", ""))

            e_prompt = playtester.build_executor_prompt(
                step_num=57954,
                batch_id=1,
                batch_step_idx=2,
                delegated_plan={"plan_goal": "Market check", "max_batch_steps": 3, "steps": [{}, {}]},
                screenshot_path=shot,
                burst_paths=[],
                ui_snapshot={"active_view": "hangar"},
                last_flight_recorder=raw_benign_fr,
                memory=mem,
                batch_history=[{"batch_step": 1, "action_type": "click_widget"}],
            )
            self.assertIn('"benign_log_exceptions":', e_prompt)
            self.assertIn('"log_exceptions":[]', e_prompt.replace(" ", ""))

            mixed_fr = {
                "log_exceptions": [minimap_item, real_item],
                "quasar_errors": [],
                "macho_exceptions": [],
            }
            partitioned = playtester.PlaytesterBugOracle.partition_flight_recorder_exceptions(mixed_fr)
            self.assertEqual(partitioned["log_exceptions"], [real_item])
            self.assertEqual(partitioned["benign_log_exceptions"], [minimap_item])
            self.assertEqual(len(mixed_fr["log_exceptions"]), 2)

    def test_evaluate_step_suppresses_channel5_for_1313_and_1299_while_preserving_real_tracebacks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            tb = root / "client_tracebacks.log"
            srv = root / "server.log"
            shot = root / "screen.png"
            sdir = root / "step"
            qdir.mkdir()
            sdir.mkdir()
            tb.write_text("", encoding="utf-8")
            srv.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")

            mem = playtester.PlaytesterMemory(root, git_head="2cc883ab")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            minimap_exc = {
                "headline": "AttributeError: 'MapSlider' object has no attribute 'minimumWidth'",
                "trace": self._MINIMAP_TRACE,
            }
            marketbase_exc = {
                "headline": "AttributeError: 'NoneType' object has no attribute 'max'",
                "trace": self._MARKETBASE_RESIZE_TRACE,
            }

            # 1. Issue #1313: Step N hits MapSlider._ResizeSlider -> Channel 1 drops it
            q1 = oracle.evaluate_step(
                step_num=57953,
                git_head="2cc883ab",
                decision={"action": {"type": "click_widget", "target": "entry_1"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": True,
                    "flight_recorder": {"log_exceptions": [minimap_exc], "quasar_errors": [], "macho_exceptions": []},
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(q1, [])
            self.assertTrue(oracle._prev_step_dropped_all_log_excs)

            # Step N+1 (even if Step N+1 action also fires MarketData._OnResize while filing #1313!)
            dec_1313 = {
                "action": {"type": "click_widget", "target": "marketDetailsTabmarketdata"},
                "previous_action_verification": {
                    "expectation_met": False,
                    "discrepancy_summary": "AttributeError: 'MapSlider' object has no attribute 'minimumWidth' in minimap.py:_ResizeSlider",
                },
                "bug_report": {
                    "should_file": True,
                    "category": "client_exception",
                    "severity": "medium",
                    "subsystem": "market / carbonui.graphs.minimap",
                    "title": "[Bug Report][market / carbonui] AttributeError: 'MapSlider' object has no attribute 'minimumWidth' in MapSlider._ResizeSlider",
                    "expected_behavior": "Selecting Miner I renders Price History cleanly",
                    "actual_behavior": "minimap.py line 111 (_ResizeSlider) AttributeError",
                    "reproduction_steps": ["Select Miner I on Price History tab"],
                },
            }
            q2 = oracle.evaluate_step(
                step_num=57954,
                git_head="2cc883ab",
                decision=dec_1313,
                exec_res={
                    "action_ok": True,
                    "flight_recorder": {"log_exceptions": [marketbase_exc], "quasar_errors": [], "macho_exceptions": []},
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "benign_log_exceptions": [minimap_exc], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(q2, [])

            # 2. Issue #1299: MarketData._OnResize (raw or pre-partitioned) + Channel 5 client_exception -> suppressed
            dec_1299 = {
                "action": {"type": "close_window", "target": "market"},
                "previous_action_verification": {
                    "expectation_met": False,
                    "discrepancy_summary": "marketbase.py:3718 _OnResize NoneType.max",
                },
                "bug_report": {
                    "should_file": True,
                    "category": "client_exception",
                    "severity": "medium",
                    "subsystem": "market_ui",
                    "title": "[Client Exception] AttributeError: 'NoneType' object has no attribute 'max' in marketbase.py:_OnResize when selecting Market Data tab",
                    "expected_behavior": "Market Data tab renders cleanly",
                    "actual_behavior": "marketbase.py line 3718 (_OnResize) NoneType.max",
                    "reproduction_steps": ["Click Market Data tab for Avatar (11567)"],
                },
            }
            q3 = oracle.evaluate_step(
                step_num=53111,
                git_head="2cc883ab",
                decision=dec_1299,
                exec_res={
                    "action_ok": True,
                    "flight_recorder": {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [marketbase_exc], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(q3, [])

            # 3. Negative checks: real non-benign traceback or unhandled tasklet crash MUST still be queued!
            real_exc = {"headline": "KeyError: 'orders'", "trace": self._REAL_TRACE}
            q_real = oracle.evaluate_step(
                step_num=57955,
                git_head="2cc883ab",
                decision=dec_1299,
                exec_res={
                    "action_ok": True,
                    "flight_recorder": {"log_exceptions": [marketbase_exc, real_exc], "quasar_errors": [], "macho_exceptions": []},
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "benign_log_exceptions": [marketbase_exc], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(q_real), 1)
            self.assertEqual(json.loads(q_real[0].read_text(encoding="utf-8"))["kind"], "client_traceback")

            tasklet_exc = {
                "headline": "UserError: ('CustomInfo', {'info': 'UnexpectedServerState'})",
                "trace": self._UNHANDLED_TASKLET_TRACE,
            }
            q_tasklet = oracle.evaluate_step(
                step_num=57956,
                git_head="2cc883ab",
                decision={"action": {"type": "click_widget", "target": "entry_1"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": True,
                    "flight_recorder": {"log_exceptions": [tasklet_exc], "quasar_errors": [], "macho_exceptions": []},
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(len(q_tasklet), 1)
            self.assertEqual(json.loads(q_tasklet[0].read_text(encoding="utf-8"))["kind"], "client_traceback")

            # Agent-diagnosed real client_exception when no benign layout race was involved -> NOT suppressed
            dec_real_agent = {
                "action": {"type": "click_widget", "target": "marketDetailsTabmarketdata"},
                "previous_action_verification": {"expectation_met": False, "discrepancy_summary": "KeyError orders"},
                "bug_report": {
                    "should_file": True,
                    "category": "client_exception",
                    "severity": "high",
                    "subsystem": "market",
                    "title": "[Market] KeyError: 'orders' in marketWindow.py:412",
                    "expected_behavior": "Orders populate cleanly",
                    "actual_behavior": "KeyError: 'orders' raised in LoadMarketOrders",
                    "reproduction_steps": ["Open market window"],
                },
            }
            q_real_agent = oracle.evaluate_step(
                step_num=57957,
                git_head="2cc883ab",
                decision=dec_real_agent,
                exec_res={"action_ok": True, "flight_recorder": {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []}},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(q_real_agent), 1)
            self.assertEqual(json.loads(q_real_agent[0].read_text(encoding="utf-8"))["kind"], "client_exception")

    def test_execute_step_fr_delta_moves_benign_log_exceptions_without_forcing_escalation(self):
        with tempfile.TemporaryDirectory() as td:
            wt = Path(td)
            args = playtester.parse_args(["--worktree", str(wt)])
            runner = playtester.PlaytesterRunner(args)
            runner.oracle.async_write = False
            runner.cached_git_head = "2cc883ab"
            snap = {
                "active_view": "hangar",
                "session": {"charid": 90000001, "stationid": 60003760},
                "ui": {"open_windows": [], "modals": [], "clickable_widgets": []},
                "open_windows": [],
                "modals": [],
                "space": {},
                "skills_state": {},
                "wallet_state": {},
            }
            minimap_exc = {
                "headline": "AttributeError: 'MapSlider' object has no attribute 'minimumWidth'",
                "trace": self._MINIMAP_TRACE,
            }

            def fake_rpc_call(method, params=None, timeout=15.0):
                if method == "get_state":
                    return dict(snap)
                if method == "execute_action":
                    return {
                        "action_ok": True,
                        "flight_recorder": {
                            "log_exceptions": [minimap_exc],
                            "quasar_errors": [],
                            "macho_exceptions": [],
                        },
                        "state_diff": {"ui_changed": True},
                    }
                return {"ok": True}

            runner.rpc = mock.Mock()
            runner.rpc.call.side_effect = fake_rpc_call
            plan_dec = {
                "observation": "Market Price History open",
                "previous_action_verification": {"expectation_met": True, "discrepancy_summary": ""},
                "bug_report": {"should_file": False},
                "current_goal": "Select Miner I",
                "memory_update": {},
                "delegated_plan": {
                    "plan_goal": "Select Miner I then close market",
                    "horizon_rationale": "2 direct steps",
                    "max_batch_steps": 2,
                    "steps": [
                        {
                            "step_number": 1,
                            "exec_mode": "direct",
                            "action": {"type": "wait", "wait_ms": 50},
                        },
                        {
                            "step_number": 2,
                            "exec_mode": "direct",
                            "action": {"type": "wait", "wait_ms": 50},
                        },
                    ],
                },
            }
            with mock.patch.object(playtester, "capture_window_screenshot", return_value={"ok": False}), \
                 mock.patch.object(playtester, "invoke_agy_planner", return_value=(plan_dec, "hydrogen-l-tf-medium", 0.2)):
                fr_delta, _ = runner.execute_step(1, {}, [])

            self.assertEqual(fr_delta.get("log_exceptions"), [])
            self.assertEqual(fr_delta.get("benign_log_exceptions"), [minimap_exc])
            self.assertFalse(runner._escalate_to_diagnostic_planner)
            self.assertIsNone(runner._pending_assertion_escalation)
            self.assertFalse(runner.batch_completed_flag)

            # Real non-benign traceback must stay in fr_delta["log_exceptions"] and escalate normally
            real_exc = {"headline": "KeyError: 'orders'", "trace": self._REAL_TRACE}
            def fake_rpc_real(method, params=None, timeout=15.0):
                if method == "get_state":
                    return dict(snap)
                if method == "execute_action":
                    return {
                        "action_ok": True,
                        "flight_recorder": {
                            "log_exceptions": [minimap_exc, real_exc],
                            "quasar_errors": [],
                            "macho_exceptions": [],
                        },
                        "state_diff": {"ui_changed": False},
                    }
                return {"ok": True}

            runner.rpc.call.side_effect = fake_rpc_real
            with mock.patch.object(playtester, "capture_window_screenshot", return_value={"ok": False}):
                fr_delta_real, _ = runner.execute_step(2, fr_delta, [])

            self.assertEqual(fr_delta_real.get("log_exceptions"), [real_exc])
            self.assertEqual(fr_delta_real.get("benign_log_exceptions"), [minimap_exc])
            self.assertTrue(runner._escalate_to_diagnostic_planner)
            self.assertIsNotNone(runner._pending_assertion_escalation)
            self.assertEqual(runner._pending_assertion_escalation.get("log_exceptions"), [real_exc])


class TestGmBridgeScatterCapture1268(unittest.TestCase):
    @staticmethod
    def _make_gm_bridge_fake(gm_events, *, fail_cmd=False):
        import types

        class _FakeGmConn:
            def __init__(self):
                self.connected = True

            def close(self):
                self.connected = False
                gm_events.append(("close",))

        class _FakeSvc:
            def Login(self, u, pw):
                gm_events.append(("Login", u, pw))
                return True

            def GetCharactersToSelect(self):
                return [{"characterID": 90000003}]

            def SelectCharacterID(self, cid):
                gm_events.append(("SelectCharacterID", cid))
                return True

            def SlashCmd(self, cmd):
                gm_events.append(("SlashCmd", cmd))
                if fail_cmd:
                    raise RuntimeError("slash boom")
                return {"ok": True}

            def AdvanceClock(self, secs, tgt=0):
                gm_events.append(("AdvanceClock", secs, tgt))
                if fail_cmd:
                    raise RuntimeError("clock boom")
                return {"ok": True}

        fake_bridge = types.ModuleType("evomu_bridge")

        def _install_hook(sm, host="127.0.0.1", port=26000):
            sm.RemoteSvc = lambda _name: _FakeSvc()
            return _FakeGmConn()

        fake_bridge.install_service_manager_hook = _install_hook
        return fake_bridge

    def test_diff_scatter_events_handles_repeats_and_sliding_window(self):
        fn = playtester.PlaytesterRunner._diff_scatter_events
        self.assertEqual(fn(["A"], None), [])
        self.assertEqual(fn(None, ["A", "B"]), ["A", "B"])
        self.assertEqual(fn(["A", "B"], ["A", "B"]), [])
        # Duplicate scatter appended after identical suffix
        self.assertEqual(
            fn(["OnSessionChanged", "OnModuleAttributeChanges"], ["OnSessionChanged", "OnModuleAttributeChanges", "OnModuleAttributeChanges"]),
            ["OnModuleAttributeChanges"],
        )
        # Full 60-element sliding window shifts oldest item out
        pre_60 = [f"Old_{i}" for i in range(60)]
        post_60 = pre_60[2:] + ["OnDamageStateChange", "OnModuleAttributeChanges"]
        self.assertEqual(fn(pre_60, post_60), ["OnDamageStateChange", "OnModuleAttributeChanges"])

    def test_gm_bridge_heal_and_advancetime_drain_scatters_before_get_state(self):
        for step_num, action, expected_gm_op in (
            (
                1,
                {"type": "slash_command", "command": "/heal"},
                ("SlashCmd", "/heal 90000001"),
            ),
            (
                2,
                {"type": "slash_command", "command": "/advancetime 30"},
                ("SlashCmd", "/advancetime 90000001 30"),
            ),
            (
                3,
                {"type": "advance_server_time", "target": "30"},
                ("AdvanceClock", 30, 90000001),
            ),
        ):
            with self.subTest(step_num=step_num, expected_gm_op=expected_gm_op):
                with tempfile.TemporaryDirectory() as td:
                    wt = Path(td)
                    args = playtester.parse_args(["--worktree", str(wt), "--persona", "gm_tester"])
                    runner = playtester.PlaytesterRunner(args)
                    runner.oracle.async_write = False
                    runner.cached_git_head = "deadbeef"

                    events = []
                    fake_bridge = self._make_gm_bridge_fake(events)
                    clock = {"now": 100.0}
                    fr_polls_after_gm = {"count": 0}
                    gm_done = {"flag": False}

                    # Intercept GM op timing via fake_bridge SlashCmd/AdvanceClock wrapper
                    orig_install = fake_bridge.install_service_manager_hook

                    def _tracked_install(sm, host="127.0.0.1", port=26000):
                        conn = orig_install(sm, host=host, port=port)
                        orig_remote = sm.RemoteSvc

                        def _wrap_remote(name):
                            svc = orig_remote(name)
                            orig_slash = svc.SlashCmd
                            orig_adv = svc.AdvanceClock

                            def _slash(cmd):
                                res = orig_slash(cmd)
                                gm_done["flag"] = True
                                return res

                            def _adv(secs, tgt=0):
                                res = orig_adv(secs, tgt)
                                gm_done["flag"] = True
                                return res

                            svc.SlashCmd = _slash
                            svc.AdvanceClock = _adv
                            return svc

                        sm.RemoteSvc = _wrap_remote
                        return conn

                    fake_bridge.install_service_manager_hook = _tracked_install

                    def fake_sleep(dt):
                        clock["now"] += float(dt)

                    snap = {
                        "active_view": "hangar",
                        "session": {"charid": 90000001, "stationid": 60003760},
                        "ui": {"open_windows": [], "modals": [], "clickable_widgets": []},
                        "open_windows": [],
                        "modals": [],
                        "space": {},
                        "skills_state": {},
                        "wallet_state": {},
                    }

                    def fake_rpc_call(method, params=None, timeout=15.0):
                        events.append(("rpc", method))
                        if method == "get_state":
                            return dict(snap)
                        if method == "get_flight_recorder":
                            if not gm_done["flag"]:
                                return {
                                    "ok": True,
                                    "result": {
                                        "macho_calls": [],
                                        "scatters_observed": ["OnSessionChanged", "OnModuleAttributeChanges"],
                                    },
                                }
                            fr_polls_after_gm["count"] += 1
                            idx = fr_polls_after_gm["count"]
                            if idx == 1:
                                scatters = ["OnSessionChanged", "OnModuleAttributeChanges"]
                            elif idx == 2:
                                scatters = [
                                    "OnSessionChanged",
                                    "OnModuleAttributeChanges",
                                    "OnDamageStateChange",
                                ]
                            else:
                                scatters = [
                                    "OnSessionChanged",
                                    "OnModuleAttributeChanges",
                                    "OnDamageStateChange",
                                    "OnModuleAttributeChanges",
                                ]
                            return {
                                "ok": True,
                                "result": {"macho_calls": [], "scatters_observed": scatters},
                            }
                        return {"ok": True}

                    runner.rpc = mock.Mock()
                    runner.rpc.call.side_effect = fake_rpc_call
                    plan_dec = {
                        "observation": "In hangar",
                        "previous_action_verification": {"expectation_met": True, "discrepancy_summary": ""},
                        "bug_report": {"should_file": False},
                        "current_goal": "Run GM action",
                        "memory_update": {},
                        "delegated_plan": {
                            "plan_goal": "GM scatter settle test",
                            "horizon_rationale": "Test gm_bridge scatter drain before get_state",
                            "max_batch_steps": 1,
                            "steps": [
                                {
                                    "step_number": 1,
                                    "exec_mode": "direct",
                                    "action": action,
                                }
                            ],
                        },
                    }

                    with mock.patch.dict(sys.modules, {"evomu_bridge": fake_bridge}), \
                         mock.patch.object(playtester, "capture_window_screenshot", return_value={"ok": False}), \
                         mock.patch.object(playtester, "invoke_agy_planner", return_value=(plan_dec, "hydrogen-l-tf-medium", 0.5)), \
                         mock.patch.object(playtester.time, "monotonic", side_effect=lambda: clock["now"]), \
                         mock.patch.object(playtester.time, "sleep", side_effect=fake_sleep):
                        fr_delta, _ = runner.execute_step(1, {}, [])
                    runner._close_gm_bridge()

                    rpc_methods = [ev[1] for ev in events if ev[0] == "rpc"]
                    self.assertNotIn("execute_action", rpc_methods)
                    self.assertEqual(
                        fr_delta.get("scatters_observed"),
                        ["OnDamageStateChange", "OnModuleAttributeChanges"],
                    )
                    self.assertGreaterEqual(fr_polls_after_gm["count"], 3)
                    gm_idx = next(i for i, ev in enumerate(events) if ev == expected_gm_op)
                    pre_fr_indices = [i for i, ev in enumerate(events) if ev == ("rpc", "get_flight_recorder") and i < gm_idx]
                    post_fr_indices = [i for i, ev in enumerate(events) if ev == ("rpc", "get_flight_recorder") and i > gm_idx]
                    # Step executes initial pre-step get_state + post-GM refreshed get_state; refreshed get_state must follow settle polls
                    post_get_state_indices = [i for i, ev in enumerate(events) if ev == ("rpc", "get_state") and i > gm_idx]
                    self.assertTrue(pre_fr_indices)
                    self.assertGreaterEqual(len(post_fr_indices), 3)
                    self.assertTrue(post_get_state_indices)
                    self.assertLess(max(post_fr_indices), min(post_get_state_indices))

    def test_gm_bridge_scatter_capture_fallbacks_when_rpc_fails_or_omits_scatters(self):
        with tempfile.TemporaryDirectory() as td:
            wt = Path(td)
            args = playtester.parse_args(["--worktree", str(wt), "--persona", "gm_tester"])
            runner = playtester.PlaytesterRunner(args)
            runner.oracle.async_write = False

            # Case 1: RPC omits scatters_observed -> [] without sleeping
            events = []
            fake_bridge = self._make_gm_bridge_fake(events)
            runner.rpc = mock.Mock()
            runner.rpc.call.return_value = {"ok": True, "macho_calls": []}
            with mock.patch.dict(sys.modules, {"evomu_bridge": fake_bridge}), \
                 mock.patch.object(playtester.time, "sleep", side_effect=AssertionError("unexpected sleep")):
                res = runner._execute_gm_or_time_control_via_bridge(
                    {"type": "slash_command", "command": "/heal"},
                    "slash_command",
                    90000001,
                )
            runner._close_gm_bridge()
            self.assertTrue(res["action_ok"])
            self.assertEqual(res["flight_recorder"]["scatters_observed"], [])

            # Case 2: get_flight_recorder raises -> graceful fallback to []
            runner.rpc = mock.Mock()
            runner.rpc.call.side_effect = RuntimeError("rpc unavailable")
            with mock.patch.dict(sys.modules, {"evomu_bridge": fake_bridge}), \
                 mock.patch.object(playtester.time, "sleep", side_effect=AssertionError("unexpected sleep")):
                res_err = runner._execute_gm_or_time_control_via_bridge(
                    {"type": "slash_command", "command": "/heal"},
                    "slash_command",
                    90000001,
                )
            runner._close_gm_bridge()
            self.assertTrue(res_err["action_ok"])
            self.assertEqual(res_err["flight_recorder"]["scatters_observed"], [])

            # Case 3: post get_flight_recorder raises after pre succeeded -> graceful fallback to []
            call_n = {"n": 0}

            def _flaky_rpc(method, params=None, timeout=5.0):
                call_n["n"] += 1
                if call_n["n"] == 1:
                    return {"scatters_observed": ["OnSessionChanged"]}
                raise ConnectionResetError("post poll failed")

            runner.rpc = mock.Mock()
            runner.rpc.call.side_effect = _flaky_rpc
            with mock.patch.dict(sys.modules, {"evomu_bridge": fake_bridge}), \
                 mock.patch.object(playtester.time, "sleep", side_effect=AssertionError("unexpected sleep")):
                res_flaky = runner._execute_gm_or_time_control_via_bridge(
                    {"type": "slash_command", "command": "/heal"},
                    "slash_command",
                    90000001,
                )
            runner._close_gm_bridge()
            self.assertTrue(res_flaky["action_ok"])
            self.assertEqual(res_flaky["flight_recorder"]["scatters_observed"], [])

            # Case 4: GM bridge RPC fails while RPC is broken -> preserves action_error and falls back cleanly
            failing_bridge = self._make_gm_bridge_fake([], fail_cmd=True)
            runner.rpc = mock.Mock()
            runner.rpc.call.side_effect = RuntimeError("rpc offline")
            with mock.patch.dict(sys.modules, {"evomu_bridge": failing_bridge}):
                res_fail = runner._execute_gm_or_time_control_via_bridge(
                    {"type": "slash_command", "command": "/heal"},
                    "slash_command",
                    90000001,
                )
            runner._close_gm_bridge()
            self.assertFalse(res_fail["action_ok"])
            self.assertIn("slash boom", res_fail["action_error"])
            self.assertEqual(res_fail["flight_recorder"]["scatters_observed"], [])


class TestBugOracleSessionTelemetryAndArgs(unittest.TestCase):
    def test_cumulative_session_telemetry_transport_errors_and_handshake_builds(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            machonet_src = root / "crates" / "eve-machonet" / "src"
            machonet_src.mkdir(parents=True)
            (machonet_src / "lib.rs").write_text(
                "pub const V24_BOOT_BUILD: u32 = 3_698_585;\n",
                encoding="utf-8",
            )
            staged_app = root / "EVE.app"
            staged_app.mkdir(parents=True)
            (staged_app / "start.ini").write_text("build = 3600000\n", encoding="utf-8")

            self.assertEqual(playtester._read_server_boot_build(root), 3698585)
            self.assertEqual(playtester._read_client_boot_build(staged_app), 3600000)

            qdir = root / "queue"
            sdir = root / "step_1"
            qdir.mkdir()
            sdir.mkdir()
            shot = root / "shot.png"
            shot.write_bytes(b"png")
            tb = root / "tb.log"
            tb.write_text("", encoding="utf-8")
            srv = root / "srv.log"
            srv.write_text(
                "2026-11-19T12:00:00Z INFO eve_server::quasar_grpc: listening on 127.0.0.1:50051\n",
                encoding="utf-8",
            )

            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            oracle.reset_log_offsets(tb, srv)
            oracle.set_handshake_builds(
                client_boot_build=playtester._read_client_boot_build(staged_app),
                server_boot_build=playtester._read_server_boot_build(root),
            )

            unseq_gateway_err = {
                "kind": "quasar",
                "service": "publicGateway",
                "method": "Send",
                "error": "Connection refused",
            }
            calls_step1 = [
                {"seq": 1, "service": "account", "method": "GetEntry", "status": "OK"},
                {"seq": 2, "service": "character", "method": "GetCharacterToSelect", "status": "OK"},
                unseq_gateway_err,
            ]

            runner = playtester.PlaytesterRunner.__new__(playtester.PlaytesterRunner)
            runner.worktree_dir = root
            runner.staged_app = staged_app
            runner.oracle = oracle
            runner.client_proc = None
            runner._session_flight_calls = []
            runner._session_flight_seqs = set()

            runner._record_session_flight_calls(calls_step1)
            # Duplicate seq / same unseq dict must not double-count.
            runner._record_session_flight_calls(calls_step1)

            queued1 = oracle.evaluate_step(
                step_num=1,
                git_head="deadbeef",
                decision={"action": {"type": "wait"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": True,
                    "flight_recorder": {
                        "macho_calls": calls_step1,
                        "log_exceptions": [],
                        "quasar_errors": [],
                        "macho_exceptions": [],
                    },
                },
                ui_snapshot={"session": {"charid": 2100000001}},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(queued1, [])
            snap1 = oracle._session_telemetry_payload()
            self.assertEqual(snap1["server_request_count"], 2)
            self.assertEqual(snap1["gateway_request_count"], 0)
            self.assertEqual(snap1["telemetry"]["gateway_transport_error_count"], 1)
            self.assertEqual(
                snap1["handshake"],
                {
                    "client_boot_build": 3600000,
                    "server_boot_build": 3698585,
                    "build_mismatch": True,
                },
            )

            # Startup Quasar log does not increment gateway count; non-startup warn or OK publicGatewaySvc does.
            with srv.open("a", encoding="utf-8") as fh:
                fh.write("2026-11-19T12:00:05Z WARN eve_server::quasar_grpc: UNAUTHENTICATED user=0\n")
            with tb.open("a", encoding="utf-8") as fh:
                fh.write(
                    'Traceback (most recent call last):\n  File "eve/client/ui/foo.py", line 10, in Bar\nRuntimeError: boom\n'
                )

            calls_step2 = [
                {"seq": 3, "service": "publicGatewaySvc", "method": "SendRequest", "status": "OK"},
            ]
            runner._record_session_flight_calls(calls_step2)
            queued2 = oracle.evaluate_step(
                step_num=2,
                git_head="deadbeef",
                decision={"action": {"type": "click"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": True,
                    "flight_recorder": {
                        "macho_calls": calls_step2,
                        "log_exceptions": [],
                        "quasar_errors": [],
                        "macho_exceptions": [],
                    },
                },
                ui_snapshot={"session": {"charid": 2100000001}},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(len(queued2), 1)
            bundle = json.loads(queued2[0].read_text(encoding="utf-8"))
            self.assertEqual(bundle["server_request_count"], 3)
            self.assertEqual(bundle["gateway_request_count"], 1)
            self.assertTrue(bundle["handshake"]["build_mismatch"])
            self.assertEqual(bundle["manual_report"]["client_build"], 3600000)
            self.assertEqual(bundle["telemetry"]["gateway_transport_error_count"], 1)

            runner.stop_client()
            snap_after_stop = oracle._session_telemetry_payload()
            self.assertEqual(snap_after_stop["server_request_count"], 0)
            self.assertEqual(snap_after_stop["gateway_request_count"], 0)
            self.assertEqual(snap_after_stop["handshake"]["client_boot_build"], 3600000)

    def test_playtester_parse_args_backwards_compatible(self):
        args = playtester.parse_args(["--scenario", "scenarios/warp.json", "--no-github-sync"])
        self.assertEqual(args.scenarios, ["scenarios/warp.json"])
        self.assertTrue(args.no_github_sync)

        default_args = playtester.parse_args([])
        self.assertEqual(default_args.scenarios, [])
        self.assertFalse(default_args.no_github_sync)

    def test_oracle_flight_call_roundtrip_to_sync_bugs_and_unknown_build_omits_mismatch(self):
        tools_dir = HARNESS_DIR.parent
        if str(tools_dir) not in sys.path:
            sys.path.insert(0, str(tools_dir))
        import sync_bugs_to_github as sync_mod

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            machonet_src = root / "crates" / "eve-machonet" / "src"
            machonet_src.mkdir(parents=True)
            (machonet_src / "lib.rs").write_text(
                "pub const V24_BOOT_BUILD: u32 = 3_698_585;\n",
                encoding="utf-8",
            )
            qdir = root / "queue"
            sdir = root / "step_1"
            qdir.mkdir()
            sdir.mkdir()
            shot = root / "shot.png"
            shot.write_bytes(b"png")
            tb = root / "tb.log"
            tb.write_text("", encoding="utf-8")
            srv = root / "srv.log"
            srv.write_text("", encoding="utf-8")

            mem = playtester.PlaytesterMemory(root, git_head="deadbeef")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            oracle.reset_log_offsets(tb, srv)

            # Unknown client_boot_build must NOT emit `"build_mismatch": False` (GH1358-03).
            snap_unknown = oracle._session_telemetry_payload()
            self.assertEqual(snap_unknown["handshake"]["server_boot_build"], 3698585)
            self.assertNotIn("build_mismatch", snap_unknown["handshake"])

            # Round-trip unstatused transport error (`unseq_gateway_err`) through evaluate_step -> sync_bugs_to_github (GH1358-04).
            unseq_gateway_err = {
                "kind": "quasar",
                "service": "publicGateway",
                "method": "Send",
                "error": "Connection refused",
            }
            oracle.record_session_flight_calls(
                [
                    {"seq": 1, "service": "account", "method": "GetEntry", "status": "OK"},
                    unseq_gateway_err,
                ]
            )
            with tb.open("a", encoding="utf-8") as fh:
                fh.write(
                    'Traceback (most recent call last):\n  File "eve/client/quasar.py", line 12, in Send\nRuntimeError: quasar publicGateway Send Connection refused\n'
                )
            queued = oracle.evaluate_step(
                step_num=1,
                git_head="deadbeef",
                decision={
                    "action": {"type": "click"},
                    "observation": "Public gateway unavailable",
                    "bug_report": {"should_file": False},
                },
                exec_res={
                    "action_ok": True,
                    "flight_recorder": {
                        "macho_calls": [unseq_gateway_err],
                        "log_exceptions": [],
                        "quasar_errors": [],
                        "macho_exceptions": [],
                    },
                },
                ui_snapshot={"session": {"charid": 2100000001}},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
            )
            self.assertEqual(len(queued), 1)
            bundle = json.loads(queued[0].read_text(encoding="utf-8"))
            self.assertEqual(len(bundle["flight_recorder"]), 1)
            fr0 = bundle["flight_recorder"][0]
            self.assertEqual(fr0["kind"], "quasar")
            self.assertEqual(fr0["error"], "Connection refused")
            self.assertEqual(fr0["status"], "ERR(Connection refused)")
            fault = sync_mod.classify_environment_fault(bundle)
            self.assertIsNotNone(fault)
            assert fault is not None
            self.assertEqual(fault[0], "zero_gateway_requests")

            # Also verify harvest_flight_recorder_anomalies preserves `kind` and `error` and `ERR(...)` status.
            oracle.record_flight_recorder_anomalies(
                [unseq_gateway_err] * 3,
                step_num=2,
                git_head="deadbeef",
                session={"charid": 2100000001},
            )
            fr_bundles = list(qdir.glob("playtester_fr_*.json"))
            self.assertEqual(len(fr_bundles), 1)
            fr_bundle = json.loads(fr_bundles[0].read_text(encoding="utf-8"))
            self.assertEqual(fr_bundle["flight_recorder"][0]["status"], "ERR(Connection refused)")
            self.assertEqual(fr_bundle["flight_recorder"][0]["kind"], "quasar")
            self.assertEqual(fr_bundle["flight_recorder"][0]["error"], "Connection refused")
            fr_fault = sync_mod.classify_environment_fault(fr_bundle)
            self.assertIsNotNone(fr_fault)
            assert fr_fault is not None
            self.assertEqual(fr_fault[0], "zero_gateway_requests")

    def test_scenario_runner_accumulates_step_flight_recorder_calls(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen_path = root / "scen.json"
            scen_path.write_text(
                json.dumps(
                    {
                        "id": "t_scen_rpc_telemetry",
                        "title": "Scenario RPC telemetry",
                        "starting_state": {},
                        "steps": [
                            {
                                "id": "step_1",
                                "action": {"type": "wait", "wait_ms": 10},
                                "checks": {"no_client_exception": False},
                            },
                            {
                                "id": "step_2",
                                "action": {"type": "wait", "wait_ms": 10},
                                "checks": {"no_client_exception": False},
                            },
                        ],
                    }
                ),
                encoding="utf-8",
            )
            args = playtester.parse_args(["--scenario", str(scen_path), "--no-github-sync"])
            args.runtime_dir = str(root / "runtime")
            args.out_dir = str(root / "out")
            runner = playtester.ScenarioRunner(args, scenario_paths=[scen_path], out_dir=root / "out")
            runner.start_server = mock.MagicMock(return_value=True)
            runner.stop_server = mock.MagicMock()

            def _mock_start_client():
                runner._last_boot_result = {
                    "state": {"session": {"charid": 90000001}},
                    "flight_recorder": {
                        "macho_calls": [
                            {"seq": 1, "service": "account", "method": "GetEntry", "status": "OK"},
                        ]
                    },
                }
                return True

            runner.start_client = mock.MagicMock(side_effect=_mock_start_client)
            captured_before_stop = {}

            def _capture_stop():
                captured_before_stop["calls"] = list(runner._session_flight_calls)
                captured_before_stop["telemetry"] = runner.oracle._session_telemetry_payload()

            runner.stop_client = mock.MagicMock(side_effect=_capture_stop)
            runner._close_gm_bridge = mock.MagicMock()
            runner._capture_failure_screenshot = mock.MagicMock(return_value=None)
            runner._run_destiny_parity_check = mock.MagicMock(return_value=None)
            step_results = iter(
                [
                    {
                        "action_ok": True,
                        "post_state": {"session": {"charid": 90000001}},
                        "flight_recorder": {
                            "macho_calls": [
                                {"seq": 2, "service": "machoNet", "method": "GetTime", "status": "OK"}
                            ]
                        },
                    },
                    {
                        "action_ok": True,
                        "post_state": {"session": {"charid": 90000001}},
                        "flight_recorder": {
                            "macho_calls": [
                                {"seq": 3, "service": "publicGatewaySvc", "method": "SendRequest", "status": "OK"}
                            ]
                        },
                    },
                ]
            )

            def _rpc_call(method, *_args, **_kwargs):
                if method == "get_state":
                    return {"state": {"session": {"charid": 90000001}}}
                if method == "execute_action":
                    return next(step_results)
                return {}

            rpc_mock = mock.MagicMock()
            rpc_mock.call.side_effect = _rpc_call
            runner.rpc = rpc_mock

            scen = playtester.load_scenario_file(scen_path)
            journey, _meta = runner.run_single_scenario(scen)
            self.assertTrue(journey["passed"], journey.get("failure"))
            self.assertEqual([c["seq"] for c in captured_before_stop["calls"]], [1, 2, 3])
            snap = captured_before_stop["telemetry"]
            self.assertEqual(snap["server_request_count"], 3)
            self.assertEqual(snap["gateway_request_count"], 1)

    def test_gh1358_r3_01_refresh_oracle_handshake_builds_clientless_staged_app(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            machonet_src = root / "crates" / "eve-machonet" / "src"
            machonet_src.mkdir(parents=True)
            (machonet_src / "lib.rs").write_text(
                "pub const V24_BOOT_BUILD: u32 = 3_698_585;\n",
                encoding="utf-8",
            )
            empty_staged_app = root / "staged_eve_app"
            empty_staged_app.mkdir(parents=True)

            # Full PlaytesterRunner construction on a clientless host (no start.ini, no client_build_<digits>)
            # must not raise TypeError during _refresh_oracle_handshake_builds().
            args = playtester.parse_args(["--no-github-sync"])
            args.worktree = str(root)
            args.runtime_dir = str(root / "runtime")
            args.staged_app = str(empty_staged_app)
            runner = playtester.PlaytesterRunner(args)
            try:
                snap = runner.oracle._session_telemetry_payload()
                self.assertEqual(snap["handshake"]["server_boot_build"], 3698585)
                self.assertNotIn("client_boot_build", snap["handshake"])
                self.assertNotIn("build_mismatch", snap["handshake"])

                # Direct call with None / 0 / non-positive start.ini also guards cleanly without TypeError.
                (empty_staged_app / "start.ini").write_text("build = 0\n", encoding="utf-8")
                runner._refresh_oracle_handshake_builds(None)
                runner._refresh_oracle_handshake_builds(0)
                snap_zero = runner.oracle._session_telemetry_payload()
                self.assertNotIn("client_boot_build", snap_zero["handshake"])
                self.assertNotIn("build_mismatch", snap_zero["handshake"])

                # Explicit positive client_build still populates handshake metadata.
                runner._refresh_oracle_handshake_builds(3698585)
                snap_ok = runner.oracle._session_telemetry_payload()
                self.assertEqual(snap_ok["handshake"]["client_boot_build"], 3698585)
                self.assertEqual(snap_ok["handshake"]["build_mismatch"], False)
            finally:
                runner.oracle.flush()

    def test_host_gm_bridge_rpc_server_and_sim_time_dedup_in_summary(self):
        import evomu_inclient_helpers as hlp

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            staged = root / "staged_eve"
            staged.mkdir(parents=True)
            args = playtester.parse_args(["--no-github-sync"])
            args.worktree = str(root)
            args.runtime_dir = str(root / "rt")
            args.staged_app = str(staged)
            runner = playtester.PlaytesterRunner(args)
            gm_calls = []

            class _FakeGmSm:
                class _TimeControl:
                    def AdvanceClock(self, secs, tgt=0):
                        gm_calls.append(("AdvanceClock", int(secs), int(tgt)))
                        return 133000000000000000

                    def ClearSessionTimer(self, tgt=0):
                        gm_calls.append(("ClearSessionTimer", int(tgt)))
                        return True

                class _Slash:
                    def SlashCmd(self, cmd):
                        gm_calls.append(("SlashCmd", str(cmd)))
                        return {"ok": True}

                class _StationSvc:
                    def Dock(self, station_id):
                        gm_calls.append(("Dock", int(station_id)))
                        return {"ok": True}

                def RemoteSvc(self, name):
                    if name == "timeControl":
                        return self._TimeControl()
                    if name == "slash":
                        return self._Slash()
                    if name == "stationSvc":
                        return self._StationSvc()
                    raise AssertionError(f"unexpected service {name}")

            try:
                port = runner._ensure_host_gm_bridge_server()
                self.assertGreater(port, 0)
                with mock.patch.object(runner, "_ensure_gm_bridge", return_value=_FakeGmSm()):
                    cfg_path = runner.write_inclient_cfg()
                    cfg = json.loads(cfg_path.read_text(encoding="utf-8"))
                    self.assertEqual(cfg["host_gm_port"], port)

                    ipc = hlp.IpcClient(port)
                    try:
                        clr_resp = ipc.request({"cmd": "clear_session_timer", "target_char_id": 90000001})
                        adv_resp = ipc.request({"cmd": "advance_clock", "secs": 15, "target_char_id": 90000001})
                        sl_resp = ipc.request({"cmd": "slash_cmd", "command": "/heal", "target_char_id": 90000001})
                        desp_resp = ipc.request({
                            "cmd": "slash_cmd",
                            "command": "/despawn 88001",
                            "target_char_id": 90000001,
                            "target_solar_system_id": 30004759,
                        })
                    finally:
                        ipc.close()

                self.assertTrue(clr_resp["ok"])
                self.assertTrue(adv_resp["ok"])
                self.assertTrue(sl_resp["ok"])
                self.assertTrue(sl_resp["handled"])
                self.assertTrue(desp_resp["ok"])
                self.assertTrue(desp_resp["handled"])
                inj_id = adv_resp["injection_id"]
                self.assertEqual(
                    gm_calls,
                    [
                        ("ClearSessionTimer", 90000001),
                        ("AdvanceClock", 15, 90000001),
                        ("SlashCmd", "/heal 90000001"),
                        ("SlashCmd", "/tr me 30004759 noblock"),
                        ("SlashCmd", "/despawn 88001"),
                        ("SlashCmd", "/tr me 60003760 noblock"),
                        ("ClearSessionTimer", 90000003),
                        ("Dock", 60003760),
                    ],
                )

                # Simulate exec_res returning the same injection_id already buffered host-side
                exec_res = {
                    "action_ok": True,
                    "sim_time_injections": [
                        {
                            "injection_id": inj_id,
                            "seconds": 15.0,
                            "target_char_id": 90000001,
                            "via": "gm_bridge",
                            "source": "inclient",
                        }
                    ],
                }
                step_sim = runner._absorb_step_sim_time(exec_res)
                self.assertEqual(step_sim["sim_time_injected_sec"], 15.0)
                self.assertEqual(step_sim["sim_time_injection_count"], 1)
                self.assertEqual(exec_res["sim_time_injected_sec"], 15.0)

                summary = runner._write_run_summary(step_num=1, final=True)
                self.assertEqual(summary["total_sim_time_injected_sec"], 15.0)
                self.assertEqual(summary["total_sim_time_injection_count"], 1)
                run_sum_file = json.loads((runner.current_run_dir / "run_summary.json").read_text(encoding="utf-8"))
                self.assertEqual(run_sum_file["total_sim_time_injected_sec"], 15.0)
                self.assertEqual(run_sum_file["total_sim_time_injection_count"], 1)

                # /fastforward 180 via slash_cmd records sim-time injection over live host bridge IPC
                with mock.patch.object(runner, "_ensure_gm_bridge", return_value=_FakeGmSm()):
                    ipc2 = hlp.IpcClient(port)
                    try:
                        ff_resp = ipc2.request({"cmd": "slash_cmd", "command": "/fastforward 180", "target_char_id": 90000001})
                    finally:
                        ipc2.close()
                self.assertTrue(ff_resp["ok"])
                self.assertEqual(ff_resp["sim_time_injected_sec"], 180.0)
                step2_sim = runner._absorb_step_sim_time({"action_ok": True})
                self.assertEqual(step2_sim["sim_time_injected_sec"], 180.0)
                summary2 = runner._write_run_summary(step_num=2, final=True)
                self.assertEqual(summary2["total_sim_time_injected_sec"], 195.0)
                self.assertEqual(summary2["total_sim_time_injection_count"], 2)

                # _SimTimeTracker ignores bool seq=True and preserves newest keys over >20000 events
                tracker = playtester._SimTimeTracker(retain_events=False)
                tracker.absorb_events([
                    {"seq": True, "seconds": 5.0, "method": "AdvanceClock"},
                    {"seq": False, "seconds": 5.0, "method": "AdvanceClock"},
                ])
                self.assertEqual(tracker.total_count, 2)
                tracker.absorb_events([
                    {"injection_id": f"k-{i}", "seconds": 1.0} for i in range(20005)
                ])
                self.assertIn(("id", "k-20004"), tracker.seen_ids)
                self.assertNotIn(("id", "k-0"), tracker.seen_ids)
            finally:
                runner._stop_host_gm_bridge_server()
                runner.oracle.flush()


class TestClusterDowntimeHandling1190(unittest.TestCase):
    def test_cluster_downtime_window_and_remaining_seconds(self):
        from datetime import datetime, timezone

        before = datetime(2026, 11, 21, 10, 59, 59, tzinfo=timezone.utc).timestamp()
        inside = datetime(2026, 11, 21, 11, 2, 0, tzinfo=timezone.utc).timestamp()
        at_end = datetime(2026, 11, 21, 11, 15, 0, tzinfo=timezone.utc).timestamp()

        self.assertFalse(playtester._is_cluster_downtime_window(now_unix=before))
        self.assertEqual(playtester._seconds_until_downtime_ends(now_unix=before), 0.0)

        self.assertTrue(playtester._is_cluster_downtime_window(now_unix=inside))
        self.assertAlmostEqual(playtester._seconds_until_downtime_ends(now_unix=inside), 785.0, places=3)

        self.assertFalse(playtester._is_cluster_downtime_window(now_unix=at_end))
        self.assertEqual(playtester._seconds_until_downtime_ends(now_unix=at_end), 0.0)

        # Midnight wrap-around edge case (23:55 -> 00:10 UTC)
        wrap_now = datetime(2026, 11, 21, 23, 58, 0, tzinfo=timezone.utc).timestamp()
        self.assertTrue(playtester._is_cluster_downtime_window(now_unix=wrap_now, downtime_utc_secs=86100, window_secs=900))
        self.assertAlmostEqual(
            playtester._seconds_until_downtime_ends(now_unix=wrap_now, downtime_utc_secs=86100, window_secs=900, buffer_secs=5.0),
            725.0,
            places=3,
        )

    def test_wait_out_cluster_downtime_sleeps_in_chunks_and_advances_offsets(self):
        from datetime import datetime, timezone

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            staged = root / "staged_eve"
            staged.mkdir(parents=True)
            args = playtester.parse_args(["--no-github-sync", "--continuous", "--no-hybrid"])
            args.worktree = str(root)
            args.staged_app = str(staged)
            runner = playtester.PlaytesterRunner(args)
            try:
                runner.current_run_dir.mkdir(parents=True, exist_ok=True)
                tb_log = runner.current_run_dir / "client_tracebacks.log"
                srv_log = runner.current_run_dir / "server.log"
                tb_log.write_text("GPSTransportClosed: Daily cluster downtime\n", encoding="utf-8")
                srv_log.write_text("INFO server starting scheduled downtime\n", encoding="utf-8")

                inside = datetime(2026, 11, 21, 11, 14, 50, tzinfo=timezone.utc).timestamp()
                outside = datetime(2026, 11, 21, 12, 0, 0, tzinfo=timezone.utc).timestamp()

                self.assertTrue(
                    runner._is_cluster_downtime_active(
                        exec_res={"action_error": "UserError(GPSTransportClosed)"},
                        now_unix=inside,
                    )
                )
                # Generic GPSTransportClosed outside 11:00-11:15 UTC without downtime message is still treated as a bug.
                tb_log.write_text("GPSTransportClosed: connection dropped\n", encoding="utf-8")
                self.assertFalse(
                    runner._is_cluster_downtime_active(
                        exec_res={"action_error": "UserError(GPSTransportClosed)"},
                        now_unix=outside,
                    )
                )
                # Explicit ClusterShuttingDown message always triggers cleanly regardless of clock skew.
                self.assertTrue(
                    runner._is_cluster_downtime_active(
                        exc=RuntimeError("UserError(ClusterShuttingDown): Cluster shutdown in progress"),
                        now_unix=outside,
                    )
                )

                clock = [inside]
                sleeps = []

                def fake_sleep(secs):
                    sleeps.append(secs)
                    clock[0] += secs

                with mock.patch.object(runner, "stop_client") as stop_mock:
                    slept = runner._wait_out_cluster_downtime(
                        reason="test daily downtime",
                        now_unix=inside,
                        sleep_fn=fake_sleep,
                        time_fn=lambda: clock[0],
                        chunk_secs=5.0,
                    )
                stop_mock.assert_called_once()
                # From 11:14:50 to 11:15:05 is 15.0s -> three 5.0s chunks.
                self.assertAlmostEqual(slept, 15.0, places=3)
                self.assertEqual(len(sleeps), 3)
                self.assertEqual(runner.oracle._traceback_offset, tb_log.stat().st_size)
                self.assertEqual(runner.oracle._server_log_offset, srv_log.stat().st_size)
            finally:
                runner.oracle.flush()

    def test_start_client_downtime_login_rejection_waits_and_retries_once(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            staged = root / "staged_eve"
            (staged / "bin64").mkdir(parents=True)
            (staged / "bin64" / "exefile").write_text("", encoding="utf-8")
            args = playtester.parse_args(["--no-github-sync", "--continuous"])
            args.worktree = str(root)
            args.staged_app = str(staged)
            runner = playtester.PlaytesterRunner(args)
            try:
                fake_proc = mock.MagicMock()
                fake_proc.pid = 42424
                fake_proc.poll.return_value = None
                bootstrap_attempts = {"n": 0}

                def fake_rpc_call(method, params=None, timeout=None):
                    if method == "bootstrap_session":
                        bootstrap_attempts["n"] += 1
                        if bootstrap_attempts["n"] == 1:
                            raise RuntimeError("ClusterShuttingDown: Cluster shutdown in progress")
                        return {"session": {"charid": 90000001}, "active_view": "hangar"}
                    return {}

                wait_calls = []
                with mock.patch.object(runner, "stop_client"), \
                     mock.patch.object(runner, "_host_gm_port_for_client", return_value=0), \
                     mock.patch.object(runner, "_crashmon_pids", return_value=set()), \
                     mock.patch("playtester.stage_playtester_client", return_value=staged), \
                     mock.patch("playtester.subprocess.Popen", return_value=fake_proc), \
                     mock.patch.object(runner.rpc, "ping", return_value=True), \
                     mock.patch.object(runner.rpc, "call", side_effect=fake_rpc_call), \
                     mock.patch.object(runner, "_wait_out_cluster_downtime", side_effect=lambda **kw: wait_calls.append(kw) or 10.0):
                    ok = runner.start_client()

                self.assertTrue(ok)
                self.assertEqual(bootstrap_attempts["n"], 2)
                self.assertEqual(len(wait_calls), 1)
            finally:
                runner.oracle.flush()

    def test_window_assertions_ignore_chat_channels_unless_targeted(self):
        chat_only_state = {
            "open_windows": [
                {"window_id": "ChatWindowStack", "name": "ChatWindowStack", "caption": "Corp", "cls": "ChatWindowStack", "populated_count": 3},
                {"window_id": "chatchannel_corp", "name": "chatchannel_corp", "caption": "Corp", "cls": "ChannelWindow", "populated_count": 3},
            ],
            "ui": {"open_windows": [], "modals": []},
        }
        # Closing the Corporation window must pass when only Corp Chat remains open.
        ok, err = playtester._evaluate_step_assertions({"expect_window_closed": "corp"}, {}, chat_only_state)
        self.assertTrue(ok, err)
        # Opening / populating "corp" must NOT falsely match chatchannel_corp.
        ok_open, _ = playtester._evaluate_step_assertions({"expect_window_open": "corp"}, {}, chat_only_state)
        self.assertFalse(ok_open)
        pop_err = playtester._check_window_populated({"window": "corp", "min_populated": 1, "require_rpc_or_data": False}, chat_only_state["open_windows"], [])
        self.assertIsNotNone(pop_err)
        # Explicit chat assertions still match chat windows normally.
        ok_chat, err_chat = playtester._evaluate_step_assertions({"expect_window_open": "chatchannel_corp"}, {}, chat_only_state)
        self.assertTrue(ok_chat, err_chat)
        # V24.02 Contacts (`addressbook`) & NES (`virtual_goods_store`) aliases match `people` & `store`.
        alias_state = {
            "open_windows": [
                {"window_id": "addressbook", "name": "addressbook", "caption": "Contacts", "cls": "AddressBookWindow", "populated_count": 5},
                {"window_id": "virtual_goods_store", "name": "virtual_goods_store", "caption": "New Eden Store", "cls": "virtual_goods_store", "populated_count": 5},
            ],
            "ui": {"open_windows": [], "modals": []},
        }
        self.assertIsNone(playtester._check_window_populated({"window": "people", "min_populated": 1, "require_rpc_or_data": False}, alias_state["open_windows"], []))
        self.assertIsNone(playtester._check_window_populated({"window": "store", "min_populated": 1, "require_rpc_or_data": False}, alias_state["open_windows"], []))
        ok_p_closed, _ = playtester._evaluate_step_assertions({"expect_window_closed": "people"}, {}, alias_state)
        self.assertFalse(ok_p_closed)

    def test_downtime_healthy_step_inside_window_not_misclassified_bf01(self):
        from datetime import datetime, timezone

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            staged = root / "staged_eve"
            staged.mkdir(parents=True)
            args = playtester.parse_args(["--no-github-sync", "--continuous", "--no-hybrid"])
            args.worktree = str(root)
            args.staged_app = str(staged)
            runner = playtester.PlaytesterRunner(args)
            try:
                runner.current_run_dir.mkdir(parents=True, exist_ok=True)
                srv_log = runner.current_run_dir / "server.log"
                srv_log.write_text(
                    "2026-11-21T11:02:00Z INFO eve_server::router::dispatch: Dispatching RPC call bootstrap_session\n",
                    encoding="utf-8",
                )
                inside = datetime(2026, 11, 21, 11, 2, 0, tzinfo=timezone.utc).timestamp()
                outside = datetime(2026, 11, 21, 12, 0, 0, tzinfo=timezone.utc).timestamp()
                healthy_exec = {
                    "action_ok": True,
                    "notes": "rpc completed",
                    "flight_recorder": {
                        "macho_calls": [{"service": "bootstrap_session", "status": "OK"}],
                        "log_exceptions": [],
                        "macho_exceptions": [],
                        "quasar_errors": [],
                        "scatters_observed": [{"event": "OnSessionChanged"}],
                    },
                }
                healthy_ui = {
                    "active_view": "hangar",
                    "modals": [],
                    "inventory": [{"name": "bootstrap_session_note", "hint": "rpc"}],
                    "ui": {"modals": [], "recent_modal_messages": []},
                }
                self.assertFalse(
                    runner._is_cluster_downtime_active(
                        exec_res=healthy_exec,
                        ui_snapshot=healthy_ui,
                        last_flight_recorder=healthy_exec["flight_recorder"],
                        now_unix=inside,
                    )
                )
                self.assertFalse(
                    runner._is_cluster_downtime_active(
                        exec_res=healthy_exec,
                        ui_snapshot=healthy_ui,
                        last_flight_recorder=healthy_exec["flight_recorder"],
                        now_unix=outside,
                    )
                )
                # 60m (10:00 UTC), 15m (10:45 UTC), and 5m (10:55 UTC) OnClusterShutdownInitiated countdown broadcasts
                # occur while cluster is healthy online; they must NOT trigger downtime pauses alone or suppress real bugs.
                warn_60m = datetime(2026, 11, 21, 10, 0, 0, tzinfo=timezone.utc).timestamp()
                warn_15m = datetime(2026, 11, 21, 10, 45, 0, tzinfo=timezone.utc).timestamp()
                for ts, rem in ((warn_60m, 3600), (warn_15m, 900)):
                    self.assertFalse(
                        runner._is_cluster_downtime_active(
                            exec_res={
                                "action_ok": True,
                                "flight_recorder": {
                                    "scatters_observed": [{"event": "OnClusterShutdownInitiated", "remaining_secs": rem}]
                                },
                            },
                            now_unix=ts,
                        )
                    )
                    self.assertFalse(
                        runner._is_cluster_downtime_active(
                            exec_res={
                                "action_ok": False,
                                "action_error": "UnimplementedRPC: stationSvc.DoSomething",
                                "flight_recorder": {
                                    "macho_exceptions": ["UnimplementedRPC"],
                                    "scatters_observed": [{"event": "OnClusterShutdownInitiated", "remaining_secs": rem}],
                                },
                            },
                            now_unix=ts,
                        )
                    )
                # Explicit shutdown message & disconnect modals (pre/post-state or modals_opened) DO trigger downtime.
                self.assertTrue(
                    runner._is_cluster_downtime_active(
                        exec_res={"flight_recorder": {"scatters_observed": [{"event": "ClusterShuttingDown"}]}},
                        now_unix=outside,
                    )
                )
                self.assertTrue(
                    runner._is_cluster_downtime_active(
                        ui_snapshot={"ui": {"modals": [{"text": "GPSTransportClosed"}]}},
                        now_unix=inside,
                    )
                )
                self.assertTrue(
                    runner._is_cluster_downtime_active(
                        post_state={"ui": {"modals": [{"text": "Connection Lost: GPSTransportClosed"}]}},
                        now_unix=inside,
                    )
                )
                self.assertTrue(
                    runner._is_cluster_downtime_active(
                        exec_res={"flight_recorder": {"modals_opened": [{"title": "Connection Lost", "text": "GPSTransportClosed"}]}},
                        now_unix=inside,
                    )
                )
            finally:
                runner.oracle.flush()

    def test_downtime_non_disconnect_bug_inside_window_not_suppressed_bf01(self):
        from datetime import datetime, timezone

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            staged = root / "staged_eve"
            staged.mkdir(parents=True)
            args = playtester.parse_args(["--no-github-sync", "--continuous", "--no-hybrid"])
            args.worktree = str(root)
            args.staged_app = str(staged)
            runner = playtester.PlaytesterRunner(args)
            try:
                runner.current_run_dir.mkdir(parents=True, exist_ok=True)
                tb_log = runner.current_run_dir / "client_tracebacks.log"
                srv_log = runner.current_run_dir / "server.log"
                srv_log.write_text(
                    "2026-11-21T11:02:00Z INFO eve_server::router::dispatch: Dispatching RPC call\n",
                    encoding="utf-8",
                )
                tb_log.write_text(
                    "Traceback (most recent call last):\nUnimplementedRPC: stationSvc.DoSomething\n",
                    encoding="utf-8",
                )
                inside = datetime(2026, 11, 21, 11, 2, 0, tzinfo=timezone.utc).timestamp()
                bug_exec = {
                    "action_ok": False,
                    "action_error": "UnimplementedRPC: stationSvc.DoSomething",
                    "flight_recorder": {
                        "macho_exceptions": ["UnimplementedRPC: stationSvc.DoSomething"],
                        "log_exceptions": ["Traceback ... UnimplementedRPC"],
                    },
                }
                self.assertFalse(
                    runner._is_cluster_downtime_active(
                        exec_res=bug_exec,
                        now_unix=inside,
                    )
                )
            finally:
                runner.oracle.flush()

    def test_session_boundary_and_run_loop_downtime_resume_bf02(self):
        self.assertTrue(
            playtester._is_session_boundary_flight_recorder(
                {"client_restarted_for_downtime": "2026-11-21T11:15:05Z"}
            )
        )
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            staged = root / "staged_eve"
            staged.mkdir(parents=True)
            args = playtester.parse_args(["--no-github-sync", "--continuous", "--no-hybrid"])
            args.worktree = str(root)
            args.staged_app = str(staged)
            runner = playtester.PlaytesterRunner(args)
            try:
                runner.current_run_dir.mkdir(parents=True, exist_ok=True)
                fake_server = mock.MagicMock()
                fake_server.poll.return_value = None
                fake_client = mock.MagicMock()
                fake_client.poll.return_value = None

                def fake_start_server(reset_db=False):
                    runner.server_proc = fake_server
                    return True

                start_client_calls = {"n": 0}

                def fake_start_client(_retry_after_downtime=True):
                    start_client_calls["n"] += 1
                    runner.client_proc = fake_client
                    return True

                def fake_stop_client():
                    runner.client_proc = None

                received_last_frs = []

                def fake_execute_step(step_num, last_fr, last_burst):
                    received_last_frs.append((step_num, dict(last_fr)))
                    if len(received_last_frs) == 1:
                        runner._downtime_pause_requested = True
                        return {"macho_calls": ["step1"]}, []
                    runner.stop_requested = True
                    return {"macho_calls": ["step2"]}, []

                with mock.patch("playtester.DEFAULT_CONTROL_FILE", root / "control"), \
                     mock.patch("playtester.DEFAULT_PID_FILE", root / "pid"), \
                     mock.patch.object(runner, "install_signal_handlers"), \
                     mock.patch.object(runner, "start_server", side_effect=fake_start_server), \
                     mock.patch.object(runner, "start_client", side_effect=fake_start_client), \
                     mock.patch.object(runner, "stop_client", side_effect=fake_stop_client), \
                     mock.patch.object(runner, "stop_server"), \
                     mock.patch.object(runner, "check_external_reload_triggers", return_value=(False, 0.0)), \
                     mock.patch.object(runner.rpc, "ping", return_value=True), \
                     mock.patch.object(runner, "_write_run_summary"), \
                     mock.patch.object(runner, "harvest_flight_recorder_anomalies"), \
                     mock.patch.object(runner, "_wait_out_cluster_downtime", side_effect=lambda **kw: fake_stop_client() or 10.0) as wait_mock, \
                     mock.patch.object(runner, "execute_step", side_effect=fake_execute_step):
                    rc = runner.run()

                self.assertEqual(rc, 0)
                self.assertEqual(wait_mock.call_count, 1)
                self.assertEqual(start_client_calls["n"], 2)
                self.assertEqual(len(received_last_frs), 2)
                _, step2_last_fr = received_last_frs[1]
                self.assertIn("client_restarted_for_downtime", step2_last_fr)
                self.assertNotIn("client_restarted_unexpectedly", step2_last_fr)
                self.assertTrue(playtester._is_session_boundary_flight_recorder(step2_last_fr))
            finally:
                runner.oracle.flush()

    def test_downtime_pre_window_countdown_and_early_exefile_exit_p2_2(self):
        from datetime import datetime, timezone

        pre_5m = datetime(2026, 11, 21, 10, 55, 0, tzinfo=timezone.utc).timestamp()
        pre_30m = datetime(2026, 11, 21, 10, 30, 0, tzinfo=timezone.utc).timestamp()
        # With pre_window_secs=1200, 10:55 UTC waits across 11:00-11:15:05 UTC (1205s total).
        self.assertAlmostEqual(
            playtester._seconds_until_downtime_ends(now_unix=pre_5m, pre_window_secs=1200),
            1205.0,
            places=3,
        )
        self.assertEqual(
            playtester._seconds_until_downtime_ends(now_unix=pre_30m, pre_window_secs=1200),
            0.0,
        )

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            staged = root / "staged_eve"
            (staged / "bin64").mkdir(parents=True)
            (staged / "bin64" / "exefile").write_text("", encoding="utf-8")
            args = playtester.parse_args(["--no-github-sync", "--continuous"])
            args.worktree = str(root)
            args.staged_app = str(staged)
            runner = playtester.PlaytesterRunner(args)
            try:
                exited_proc = mock.MagicMock()
                exited_proc.pid = 42421
                exited_proc.poll.return_value = 1
                exited_proc.returncode = 1

                healthy_proc = mock.MagicMock()
                healthy_proc.pid = 42422
                healthy_proc.poll.return_value = None

                popen_calls = {"n": 0}

                def fake_popen(*a, **kw):
                    popen_calls["n"] += 1
                    return exited_proc if popen_calls["n"] == 1 else healthy_proc

                wait_calls = []
                with mock.patch.object(runner, "stop_client"), \
                     mock.patch.object(runner, "_host_gm_port_for_client", return_value=0), \
                     mock.patch.object(runner, "_crashmon_pids", return_value=set()), \
                     mock.patch("playtester.stage_playtester_client", return_value=staged), \
                     mock.patch("playtester.subprocess.Popen", side_effect=fake_popen), \
                     mock.patch("playtester._is_cluster_downtime_window", return_value=True), \
                     mock.patch.object(runner.rpc, "ping", return_value=True), \
                     mock.patch.object(
                         runner.rpc,
                         "call",
                         return_value={"session": {"charid": 90000001}, "active_view": "hangar"},
                     ), \
                     mock.patch.object(runner, "_wait_out_cluster_downtime", side_effect=lambda **kw: wait_calls.append(kw) or 10.0):
                    ok = runner.start_client()

                self.assertTrue(ok)
                self.assertEqual(popen_calls["n"], 2)
                self.assertEqual(len(wait_calls), 1)
            finally:
                runner.oracle.flush()

    def test_downtime_countdown_warnings_and_post_state_modal_in_execute_step_bf01_bf02(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            staged = root / "staged_eve"
            staged.mkdir(parents=True)
            args = playtester.parse_args(["--no-github-sync", "--continuous"])
            args.worktree = str(root)
            args.staged_app = str(staged)
            runner = playtester.PlaytesterRunner(args)
            try:
                runner.current_run_dir.mkdir(parents=True, exist_ok=True)
                runner.batch_id = 1
                runner.batch_step_index = 0
                runner.batch_completed_flag = False
                runner.active_batch_plan = {
                    "plan_goal": "Test wait steps",
                    "max_batch_steps": 6,
                    "steps": [
                        {
                            "action_type": "wait",
                            "wait_ms": 10,
                            "expected_ui_and_state_change": "wait",
                        }
                        for _ in range(6)
                    ],
                }
                fake_client = mock.MagicMock()
                fake_client.pid = 42425
                fake_client.poll.return_value = None
                runner.client_proc = fake_client

                pre_state = {
                    "active_view": "hangar",
                    "modals": [],
                    "open_windows": [],
                    "clickable_widgets": [],
                    "ui": {"modals": [], "recent_modal_messages": []},
                    "space": {},
                    "skills_state": {},
                    "wallet_state": {},
                }
                action_results = [
                    # Step 1: 60m countdown warning on healthy step -> MUST NOT pause or skip oracle.
                    {
                        "ok": True,
                        "action_ok": True,
                        "flight_recorder": {
                            "scatters_observed": ["OnClusterShutdownInitiated"],
                            "macho_exceptions": [],
                            "log_exceptions": [],
                            "quasar_errors": [],
                            "modals_opened": [],
                        },
                    },
                    # Step 2: 15m countdown warning alongside a real bug -> MUST still invoke oracle.
                    {
                        "ok": False,
                        "action_ok": False,
                        "action_error": "UnimplementedRPC: stationSvc.DoSomething",
                        "flight_recorder": {
                            "scatters_observed": ["OnClusterShutdownInitiated"],
                            "macho_exceptions": [{"headline": "UnimplementedRPC: stationSvc.DoSomething"}],
                            "log_exceptions": [],
                            "quasar_errors": [],
                            "modals_opened": [],
                        },
                    },
                    # Step 3: Disconnect modal appears ONLY in post_state (popped before checks) inside downtime.
                    {
                        "ok": True,
                        "action_ok": True,
                        "post_state": {
                            "active_view": "hangar",
                            "modals": [{"title": "Connection Lost", "text": "GPSTransportClosed"}],
                            "ui": {"modals": [{"title": "Connection Lost", "text": "GPSTransportClosed"}]},
                        },
                        "flight_recorder": {
                            "scatters_observed": [],
                            "macho_exceptions": [],
                            "log_exceptions": [],
                            "quasar_errors": [],
                            "modals_opened": [{"title": "Connection Lost", "text": "GPSTransportClosed"}],
                        },
                    },
                ]
                action_idx = {"i": 0}

                def fake_rpc_call(method, params=None, timeout=None):
                    if method == "get_state":
                        return dict(pre_state)
                    if method == "execute_action":
                        idx = action_idx["i"]
                        action_idx["i"] += 1
                        return dict(action_results[idx])
                    return {}

                def fake_capture(path, **kw):
                    Path(path).write_bytes(b"png")
                    return Path(path), {}

                with mock.patch("playtester.get_git_head", return_value="deadbeef"), \
                     mock.patch("playtester.capture_window_screenshot", side_effect=fake_capture), \
                     mock.patch.object(runner.rpc, "call", side_effect=fake_rpc_call), \
                     mock.patch.object(runner.oracle, "evaluate_step") as eval_mock:
                    with mock.patch("playtester._is_cluster_downtime_window", return_value=False):
                        runner.execute_step(1, {}, [])
                        self.assertFalse(getattr(runner, "_downtime_pause_requested", False))
                        self.assertEqual(eval_mock.call_count, 1)

                        runner.batch_completed_flag = False
                        runner._escalate_to_diagnostic_planner = False
                        runner.execute_step(2, {}, [])
                        self.assertFalse(getattr(runner, "_downtime_pause_requested", False))
                        self.assertEqual(eval_mock.call_count, 2)
                        self.assertTrue(runner._escalate_to_diagnostic_planner)

                    with mock.patch("playtester._is_cluster_downtime_window", return_value=True):
                        runner.batch_completed_flag = False
                        runner._escalate_to_diagnostic_planner = False
                        runner._pending_assertion_escalation = None
                        runner.execute_step(3, {}, [])
                        self.assertTrue(runner._downtime_pause_requested)
                        self.assertFalse(runner._escalate_to_diagnostic_planner)
                        self.assertIsNone(runner._pending_assertion_escalation)
                        self.assertEqual(eval_mock.call_count, 2)
            finally:
                runner.oracle.flush()

    def test_inter_step_client_death_during_downtime_sets_downtime_fr_p2_2(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            staged = root / "staged_eve"
            staged.mkdir(parents=True)
            args = playtester.parse_args(["--no-github-sync", "--continuous", "--no-hybrid"])
            args.worktree = str(root)
            args.staged_app = str(staged)
            runner = playtester.PlaytesterRunner(args)
            try:
                runner.current_run_dir.mkdir(parents=True, exist_ok=True)
                fake_server = mock.MagicMock()
                fake_server.poll.return_value = None
                dead_client = mock.MagicMock()
                dead_client.poll.return_value = 1
                healthy_client = mock.MagicMock()
                healthy_client.poll.return_value = None

                start_calls = {"n": 0}

                def fake_start_server(reset_db=False):
                    runner.server_proc = fake_server
                    return True

                def fake_start_client(_retry_after_downtime=True):
                    start_calls["n"] += 1
                    # Simulate client dying cleanly between initial boot and before step 1 during downtime.
                    runner.client_proc = dead_client if start_calls["n"] == 1 else healthy_client
                    return True

                def fake_stop_client():
                    runner.client_proc = None

                received = []

                def fake_execute_step(step_num, last_fr, last_burst):
                    received.append(dict(last_fr))
                    runner.stop_requested = True
                    return {"macho_calls": ["ok"]}, []

                with mock.patch("playtester.DEFAULT_CONTROL_FILE", root / "control"), \
                     mock.patch("playtester.DEFAULT_PID_FILE", root / "pid"), \
                     mock.patch.object(runner, "install_signal_handlers"), \
                     mock.patch.object(runner, "start_server", side_effect=fake_start_server), \
                     mock.patch.object(runner, "start_client", side_effect=fake_start_client), \
                     mock.patch.object(runner, "stop_client", side_effect=fake_stop_client), \
                     mock.patch.object(runner, "stop_server"), \
                     mock.patch.object(runner, "check_external_reload_triggers", return_value=(False, 0.0)), \
                     mock.patch.object(runner.rpc, "ping", return_value=True), \
                     mock.patch.object(runner, "_write_run_summary"), \
                     mock.patch.object(runner, "harvest_flight_recorder_anomalies"), \
                     mock.patch("playtester._is_cluster_downtime_window", return_value=True), \
                     mock.patch.object(runner, "_wait_out_cluster_downtime", side_effect=lambda **kw: fake_stop_client() or 10.0) as wait_mock, \
                     mock.patch.object(runner, "execute_step", side_effect=fake_execute_step):
                    rc = runner.run()

                self.assertEqual(rc, 0)
                self.assertEqual(wait_mock.call_count, 1)
                self.assertEqual(start_calls["n"], 2)
                self.assertEqual(len(received), 1)
                self.assertIn("client_restarted_for_downtime", received[0])
                self.assertNotIn("client_restarted_unexpectedly", received[0])
            finally:
                runner.oracle.flush()


class TestMachoClientUserErrorDedup(unittest.TestCase):
    def test_extract_user_error_key_variants(self):
        ext = playtester.PlaytesterBugOracle.extract_user_error_key
        self.assertEqual(
            ext("User error, msg=ModuleFitFailed, dict={'reason': 'no slot'}"),
            "ModuleFitFailed",
        )
        self.assertEqual(
            ext("UserError: User error, msg=CannotFitStackedModule, dict={}"),
            "CannotFitStackedModule",
        )
        self.assertEqual(
            ext("UserError('ModuleFitFailed', {'reason': 'no slot'})"),
            "ModuleFitFailed",
        )
        self.assertEqual(
            ext("UserError(u'CannotFitStackedModule', {})"),
            "CannotFitStackedModule",
        )
        self.assertEqual(ext("UserError(CannotLoadNotEnoughCharges)"), "CannotLoadNotEnoughCharges")
        self.assertEqual(ext("UserError: CannotFitStackedModule"), "CannotFitStackedModule")
        self.assertEqual(
            ext("UserError('CustomInfo', {'info': 'TargetingFailed: Target distance exceeds max'})"),
            "CustomInfo:TargetingFailed",
        )
        self.assertEqual(
            ext("UserError: ('CustomInfo', {'info': 'TargetingFailed: Target distance exceeds max'})"),
            "CustomInfo:TargetingFailed",
        )
        self.assertEqual(
            ext("UserError: User error, msg=CustomInfo, dict={'info': u'TargetingFailed: Target distance exceeds max'}"),
            "CustomInfo:TargetingFailed",
        )
        self.assertEqual(
            ext("UserError('CustomInfo', {'info': 'Fitting failed:  slot  full'})"),
            ext("UserError: User error, msg=CustomInfo, dict={'info': u'Fitting failed: slot full'}"),
        )
        self.assertEqual(
            ext("UserError(CustomInfo: Fitting failed: slot full)"),
            ext("UserError('CustomInfo', {'info': 'Fitting failed: slot full'})"),
        )
        self.assertEqual(
            ext("UserError: CustomInfo: Fitting failed: slot full"),
            "CustomInfo:fitting failed: slot full",
        )
        self.assertEqual(
            ext("UserError('CustomInfo', {'info': ('SlotFull', {'slot': 1})})"),
            ext("UserError: ('CustomInfo', {'info': (u'SlotFull', {'slot': 1})})"),
        )
        self.assertIsNone(ext("KeyError: 'shipID'"))

    def test_extract_client_user_error_key_rejects_non_user_error_traces(self):
        cext = playtester.PlaytesterBugOracle.extract_client_user_error_key
        keyerr_with_locals = (
            'Traceback (most recent call last):\n'
            '  File "packages/fitting/ui/slotController.py", line 142, in TryFit\n'
            "    raise UserError('ModuleFitFailed')\n"
            "KeyError: 'shipID'\n"
            "Local variables:\n"
            "  last_err = \"User error, msg=ModuleFitFailed\"\n"
            "EXCEPTION END\n"
        )
        self.assertIsNone(cext(keyerr_with_locals))

        attrerr_with_thread_locals = (
            'Traceback (most recent call last):\n'
            '  File "packages/fitting/ui/slotController.py", line 150, in Refresh\n'
            "AttributeError: 'NoneType' object has no attribute 'GetItem'\n"
            "Thread Locals:\n"
            "  pending = UserError('ModuleFitFailed')\n"
        )
        self.assertIsNone(cext(attrerr_with_thread_locals))

        truncated_frame_only = (
            'Traceback (most recent call last):\n'
            '  File "packages/fitting/ui/slotController.py", line 142, in TryFit\n'
            "    raise UserError('ModuleFitFailed')\n"
        )
        self.assertIsNone(cext(truncated_frame_only))

        formatted_keyerr = (
            "Formatted exception info: KeyError: 'shipID'\n"
            "Local variables:\n"
            "  err = UserError('ModuleFitFailed')\n"
        )
        self.assertIsNone(cext(formatted_keyerr))
        self.assertIsNone(cext("RuntimeError: Unexpected UserError: ('ModuleFitFailed',)"))
        self.assertIsNone(cext('[Playtester] Client exception: KeyError: "UserError(\'ModuleFitFailed\')"'))
        self.assertEqual(
            cext(
                "[evomu_site] logmodule.LogException\n"
                'Traceback (most recent call last):\n'
                '  File "packages/fitting/ui/slotController.py", line 142, in TryFit\n'
                "UserError: ('ModuleFitFailed', {'moduleName': ''})\n"
                "Thread Locals:\n"
                "  self = <SlotController>\n"
            ),
            "ModuleFitFailed",
        )
        self.assertEqual(
            cext("Formatted exception info: UserError: ('CustomInfo', {'info': 'CanNotFit: reason'})\nThread Locals:\n  x = 1\n"),
            "CustomInfo:CanNotFit",
        )

    def test_evaluate_step_dedups_same_step_macho_and_client_user_error_and_suppresses_step_n_plus_1(self):
        sync_mod = playtester.sync_bugs_to_github
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            tb = root / "client_tracebacks.log"
            srv = root / "server.log"
            shot = root / "screen.png"
            sdir = root / "step_96364"
            qdir.mkdir()
            sdir.mkdir()
            tb.write_text("", encoding="utf-8")
            srv.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")

            mem = playtester.PlaytesterMemory(root, git_head="3c6b4a75c")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            client_trace = (
                'Traceback (most recent call last):\n'
                '  File "packages/fitting/ui/slotController.py", line 142, in TryFit\n'
                "UserError: User error, msg=ModuleFitFailed, dict={'reason': 'No high slots'}\n"
            )
            step_fr = {
                "log_exceptions": [
                    {
                        "headline": "User error, msg=ModuleFitFailed, dict={'reason': 'No high slots'}",
                        "trace": client_trace,
                    }
                ],
                "quasar_errors": [],
                "macho_exceptions": [
                    {
                        "service": "dogmaIM",
                        "method": "Fit",
                        "error": "UserError('ModuleFitFailed', {'reason': 'No high slots'})",
                    }
                ],
                "macho_calls": [
                    {
                        "offset_ms": -12,
                        "service": "dogmaIM",
                        "method": "Fit",
                        "args_summary": "(1001, 2002)",
                        "status": "UserError('ModuleFitFailed')",
                        "duration_micros": 4200,
                    }
                ],
                "traceback": client_trace,
            }
            queued = oracle.evaluate_step(
                step_num=96364,
                git_head="3c6b4a75c",
                decision={"action": {"type": "click_widget", "target": "fitBtn"}, "bug_report": {"should_file": False}},
                exec_res={"action_ok": False, "action_error": "", "flight_recorder": step_fr},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(queued), 1)
            bundle = json.loads(queued[0].read_text(encoding="utf-8"))
            self.assertEqual(bundle["kind"], "protocol_error")
            self.assertEqual(bundle["subsystem"], "macho")
            self.assertIn("dogmaIM", bundle["summary"])
            self.assertIn("ModuleFitFailed", bundle["summary"])
            self.assertIn("slotController.py", bundle["trace"])
            self.assertEqual(bundle["trace"].count("Traceback (most recent call last):"), 1)
            self.assertFalse(oracle._prev_step_dropped_all_log_excs)

            rendered = sync_mod.format_markdown_issue(bundle)
            self.assertIn("### 🐍 Client Python Traceback", rendered)
            self.assertIn("slotController.py", rendered)
            self.assertIn("MachoNet RPC Flight Recorder", rendered)
            self.assertIn("dogmaIM", rendered)

            clean_fr = {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []}
            for cat in ("protocol_error", "client_exception", "broken_game_mechanic", "ui_state_desync"):
                suppressed = oracle.evaluate_step(
                    step_num=96365,
                    git_head="3c6b4a75c",
                    decision={
                        "action": {"type": "wait", "duration_ms": 200},
                        "previous_action_verification": {
                            "expectation_met": False,
                            "discrepancy_summary": "ModuleFitFailed rejected fit",
                        },
                        "bug_report": {
                            "should_file": True,
                            "category": cat,
                            "title": f"Follow-up {cat} for ModuleFitFailed",
                            "severity": "high",
                            "subsystem": "macho" if cat != "client_exception" else "client-python",
                            "expected_behavior": "Fits module",
                            "actual_behavior": "ModuleFitFailed",
                            "reproduction_steps": ["Fit module"],
                        },
                    },
                    exec_res={"action_ok": True, "flight_recorder": clean_fr},
                    ui_snapshot={},
                    screenshot_path=shot,
                    burst_paths=[],
                    traceback_log=tb,
                    server_log=srv,
                    step_dir=sdir,
                    last_flight_recorder=step_fr,
                )
                self.assertEqual(suppressed, [], f"Expected Step N+1 {cat} report to be suppressed")

            auto_desync_suppressed = oracle.evaluate_step(
                step_num=96366,
                git_head="3c6b4a75c",
                decision={
                    "action": {"type": "wait", "duration_ms": 200},
                    "previous_action_verification": {
                        "expectation_met": False,
                        "discrepancy_summary": "Fit did not change module slot",
                    },
                    "bug_report": {"should_file": False},
                },
                exec_res={"action_ok": True, "flight_recorder": clean_fr},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder=step_fr,
            )
            self.assertEqual(auto_desync_suppressed, [])

    def test_evaluate_step_dedups_custominfo_via_client_tracebacks_log_without_duplicating_trace(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            tb = root / "client_tracebacks.log"
            srv = root / "server.log"
            shot = root / "screen.png"
            sdir = root / "step_96500"
            qdir.mkdir()
            sdir.mkdir()
            tb.write_text("", encoding="utf-8")
            srv.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")

            mem = playtester.PlaytesterMemory(root, git_head="3c6b4a75c")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            tb_block = (
                "Traceback (most recent call last):\n"
                '  File "/workspace/eve/client/script/ui/shared/fitting/fittingMgmt.py", line 88, in Fit\n'
                "UserError: User error, msg=CustomInfo, dict={'info': u'CannotFitStackedModule: max 1'}\n"
            )
            tb.write_text(tb_block, encoding="utf-8")

            queued = oracle.evaluate_step(
                step_num=96500,
                git_head="3c6b4a75c",
                decision={"action": {"type": "click_widget", "target": "fitModule"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": False,
                    "flight_recorder": {
                        "log_exceptions": [],
                        "quasar_errors": [],
                        "macho_exceptions": [
                            {
                                "service": "dogmaIM",
                                "method": "Fit",
                                "error": "UserError('CustomInfo', {'info': 'CannotFitStackedModule: max 1'})",
                            }
                        ],
                    },
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(queued), 1)
            bundle = json.loads(queued[0].read_text(encoding="utf-8"))
            self.assertEqual(bundle["kind"], "protocol_error")
            self.assertEqual(bundle["subsystem"], "macho")
            self.assertEqual(bundle["trace"].count("Traceback (most recent call last):"), 1)
            self.assertIn("fittingMgmt.py", bundle["trace"])

    def test_evaluate_step_keeps_unrelated_keyerror_mismatched_user_error_and_quasar_separate(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            tb = root / "client_tracebacks.log"
            srv = root / "server.log"
            shot = root / "screen.png"
            sdir = root / "step_96600"
            qdir.mkdir()
            sdir.mkdir()
            tb.write_text("", encoding="utf-8")
            srv.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")

            mem = playtester.PlaytesterMemory(root, git_head="3c6b4a75c")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            # 1. Unrelated KeyError traceback even when exec_res.action_error mentions ModuleFitFailed.
            q1 = oracle.evaluate_step(
                step_num=96601,
                git_head="3c6b4a75c",
                decision={"action": {"type": "click_widget", "target": "fitBtn"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": False,
                    "action_error": "Client action rejected with UserError('ModuleFitFailed', {})",
                    "flight_recorder": {
                        "log_exceptions": [
                            {
                                "headline": "KeyError: 'shipID'",
                                "trace": 'Traceback (most recent call last):\n  File "hud.py", line 10, in Draw\nKeyError: \'shipID\'\n',
                            }
                        ],
                        "quasar_errors": [],
                        "macho_exceptions": [
                            {
                                "service": "dogmaIM",
                                "method": "Fit",
                                "error": "UserError('ModuleFitFailed', {'reason': 'No slot'})",
                            }
                        ],
                    },
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(q1), 2)

            # 2. Mismatched UserError keys stay separate.
            q2 = oracle.evaluate_step(
                step_num=96602,
                git_head="3c6b4a75c",
                decision={"action": {"type": "click_widget", "target": "fitBtn2"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": False,
                    "flight_recorder": {
                        "log_exceptions": [
                            {
                                "headline": "User error, msg=CannotFitStackedModule, dict={}",
                                "trace": 'Traceback (most recent call last):\n  File "slot2.py", line 12, in Fit\nUserError: User error, msg=CannotFitStackedModule\n',
                            }
                        ],
                        "quasar_errors": [],
                        "macho_exceptions": [
                            {
                                "service": "dogmaIM",
                                "method": "Fit2",
                                "error": "UserError('ModuleFitFailed', {'reason': 'Different reason'})",
                            }
                        ],
                    },
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(q2), 2)

            # 3. Quasar error must never absorb client_traceback even if text contains a UserError key.
            q3 = oracle.evaluate_step(
                step_num=96603,
                git_head="3c6b4a75c",
                decision={"action": {"type": "click_widget", "target": "qBtn"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": False,
                    "flight_recorder": {
                        "log_exceptions": [
                            {
                                "headline": "User error, msg=QuasarRejectedReq, dict={}",
                                "trace": 'Traceback (most recent call last):\n  File "q.py", line 5, in Call\nUserError: User error, msg=QuasarRejectedReq\n',
                            }
                        ],
                        "quasar_errors": [
                            {
                                "kind": "quasar",
                                "service": "gateway",
                                "method": "Send",
                                "error": "UserError('QuasarRejectedReq', {})",
                            }
                        ],
                        "macho_exceptions": [],
                    },
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(q3), 2)

    def test_evaluate_step_never_absorbs_non_user_error_client_traceback_with_locals(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            tb = root / "client_tracebacks.log"
            srv = root / "server.log"
            shot = root / "screen.png"
            sdir = root / "step_96701"
            qdir.mkdir()
            sdir.mkdir()
            tb.write_text("", encoding="utf-8")
            srv.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")

            mem = playtester.PlaytesterMemory(root, git_head="3c6b4a75c")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            keyerr_trace = (
                'Traceback (most recent call last):\n'
                '  File "packages/fitting/ui/slotController.py", line 142, in TryFit\n'
                "    raise UserError('ModuleFitFailed')\n"
                "KeyError: 'shipID'\n"
                "Local variables:\n"
                "  last_err = \"User error, msg=ModuleFitFailed\"\n"
                "EXCEPTION END\n"
            )
            queued = oracle.evaluate_step(
                step_num=96701,
                git_head="3c6b4a75c",
                decision={"action": {"type": "click_widget", "target": "fitBtn"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": False,
                    "flight_recorder": {
                        "log_exceptions": [
                            {
                                "headline": "[Playtester] Client exception: KeyError: 'shipID'",
                                "trace": keyerr_trace,
                            }
                        ],
                        "quasar_errors": [],
                        "macho_exceptions": [
                            {
                                "service": "dogmaIM",
                                "method": "Fit",
                                "error": "UserError('ModuleFitFailed', {'reason': 'Low CPU'})",
                            }
                        ],
                    },
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(queued), 2)
            bundles = [json.loads(p.read_text(encoding="utf-8")) for p in queued]
            kinds = {b["kind"] for b in bundles}
            self.assertEqual(kinds, {"protocol_error", "client_traceback"})

    def test_evaluate_step_prev_tb_concatenated_blocks_and_dropped_benign_log_excs(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            tb = root / "client_tracebacks.log"
            srv = root / "server.log"
            shot = root / "screen.png"
            sdir = root / "step_96702"
            qdir.mkdir()
            sdir.mkdir()
            tb.write_text("", encoding="utf-8")
            srv.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")

            mem = playtester.PlaytesterMemory(root, git_head="3c6b4a75c")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            # Case A: Two concatenated plain `Traceback (most recent call last):` blocks in raw_prev_tb
            # (UserError + KeyError) must split properly so prev_tb_unexplained=True and Step N+1 is NOT suppressed.
            concat_tb = (
                'Traceback (most recent call last):\n'
                '  File "packages/fitting/ui/slotController.py", line 10, in TryFit\n'
                "UserError: ('ModuleFitFailed', {})\n"
                'Traceback (most recent call last):\n'
                '  File "packages/fitting/ui/slotController.py", line 22, in Refresh\n'
                "KeyError: 'boom'\n"
            )
            prev_fr_concat = {
                "log_exceptions": [],
                "quasar_errors": [],
                "macho_exceptions": [
                    {
                        "service": "dogmaIM",
                        "method": "Fit",
                        "error": "UserError('ModuleFitFailed', {})",
                    }
                ],
                "traceback": concat_tb,
            }
            not_suppressed = oracle.evaluate_step(
                step_num=96702,
                git_head="3c6b4a75c",
                decision={
                    "action": {"type": "wait", "duration_ms": 200},
                    "previous_action_verification": {
                        "expectation_met": False,
                        "discrepancy_summary": "KeyError boom after fit",
                    },
                    "bug_report": {
                        "should_file": True,
                        "category": "client_exception",
                        "title": "Client KeyError boom after fit",
                        "severity": "high",
                        "subsystem": "client-python",
                        "expected_behavior": "Window refreshes",
                        "actual_behavior": "KeyError: 'boom'",
                        "reproduction_steps": ["Fit module"],
                    },
                },
                exec_res={"action_ok": True, "flight_recorder": {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []}},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder=prev_fr_concat,
            )
            self.assertEqual(len(not_suppressed), 1)
            self.assertEqual(json.loads(not_suppressed[0].read_text(encoding="utf-8"))["kind"], "client_exception")

            # Case B: Step N drops ONLY a benign log_exc (_prev_step_dropped_all_log_excs=True)
            # alongside a macho UserError whose raw_prev_tb matches; Step N+1 client_exception must still be suppressed.
            oracle2 = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
            benign_trace = (
                'Traceback (most recent call last):\n'
                '  File "eve/client/script/ui/shared/market/marketbase.py", line 100, in _OnResize\n'
                "AttributeError: 'NoneType' object has no attribute 'max'\n"
            )
            step_n_fr = {
                "log_exceptions": [
                    {
                        "headline": "AttributeError: 'NoneType' object has no attribute 'max'",
                        "trace": benign_trace,
                    }
                ],
                "quasar_errors": [],
                "macho_exceptions": [
                    {
                        "service": "dogmaIM",
                        "method": "Fit",
                        "error": "UserError('ModuleFitFailed', {})",
                    }
                ],
                "traceback": (
                    'Traceback (most recent call last):\n'
                    '  File "packages/fitting/ui/slotController.py", line 10, in TryFit\n'
                    "UserError: ('ModuleFitFailed', {})\n"
                ),
            }
            q_step_n = oracle2.evaluate_step(
                step_num=96703,
                git_head="3c6b4a75c",
                decision={"action": {"type": "click_widget", "target": "fitBtn"}, "bug_report": {"should_file": False}},
                exec_res={"action_ok": False, "flight_recorder": step_n_fr},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(q_step_n), 1)
            self.assertTrue(oracle2._prev_step_dropped_all_log_excs)

            suppressed = oracle2.evaluate_step(
                step_num=96704,
                git_head="3c6b4a75c",
                decision={
                    "action": {"type": "wait", "duration_ms": 200},
                    "previous_action_verification": {
                        "expectation_met": False,
                        "discrepancy_summary": "Modal showed ModuleFitFailed",
                    },
                    "bug_report": {
                        "should_file": True,
                        "category": "client_exception",
                        "title": "Follow-up client_exception for ModuleFitFailed",
                        "severity": "high",
                        "subsystem": "client-python",
                        "expected_behavior": "Fits module",
                        "actual_behavior": "ModuleFitFailed",
                        "reproduction_steps": ["Fit module"],
                    },
                },
                exec_res={"action_ok": True, "flight_recorder": {"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []}},
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder=step_n_fr,
            )
            self.assertEqual(suppressed, [])

    def test_extract_client_user_error_key_custom_info_with_secondary_error_names(self):
        ext = playtester.PlaytesterBugOracle.extract_client_user_error_key
        self.assertEqual(
            ext(
                "User error, msg=CustomInfo, dict={'info': 'FittingError: no slots'}"
            ),
            "CustomInfo:FittingError",
        )
        self.assertEqual(
            ext(
                "UserError('CustomInfo', {'info': 'ValidationError: slot full'})"
            ),
            "CustomInfo:ValidationError",
        )
        # Non-UserError primary exception mentioning CustomInfo in locals must still reject.
        self.assertIsNone(
            ext(
                "KeyError: \"User error, msg=CustomInfo, dict={'info': 'FittingError: no slots'}\""
            )
        )

    def test_expected_user_error_module_fit_failed_precondition_verified(self):
        oracle_cls = playtester.PlaytesterBugOracle
        untrained_ui_state = {
            "skills_state": {
                "total_trained_skills": 2,
                "trained_skill_levels": {"3300": 4, "3412": 0},
            }
        }
        trained_ui_state = {
            "skills_state": {
                "total_trained_skills": 2,
                "trained_skill_levels": {"3412": 1, "3416": 2},
            }
        }
        empty_skills_ui_state = {"skills_state": {"total_trained_skills": 0, "trained_skill_levels": {}}}

        skill_3416_err = "UserError('ModuleFitFailed', {'reason': 'Missing required skill 3416 level 1 (trained: 0) for module Small Shield Booster I (type 399)', 'moduleName': 'Small Shield Booster I'})"
        skill_3412_err = "UserError('ModuleFitFailed', {'reason': 'Missing required skill 3412 level 1 (trained: 0) for module Core Probe Launcher I (type 17938)', 'moduleName': 'Core Probe Launcher I'})"
        skill_3412_lvl3_err = "UserError('ModuleFitFailed', {'reason': 'Missing required skill 3412 level 3 (trained: 1) for module Expanded Probe Launcher I (type 17901)', 'moduleName': 'Expanded Probe Launcher I'})"

        # 1. Verified untrained skill against live ui_state.skills_state -> exempted + verified telemetry detail.
        for err, want_skill in ((skill_3416_err, 3416), (skill_3412_err, 3412)):
            ok, det = oracle_cls.classify_expected_user_error(
                {"error": err}, ui_state=untrained_ui_state
            )
            self.assertTrue(ok, err)
            self.assertIsNotNone(det)
            self.assertEqual(det["key"], "ModuleFitFailed")
            self.assertEqual(det["mechanism"], "precondition_verified")
            self.assertEqual(det["precondition"], "missing_required_skill")
            self.assertTrue(det["verified"])
            self.assertEqual(det["skill_id"], want_skill)

        # Partial trained level (live=1 == reported=1 < req=3) also verifies cleanly.
        ok_lvl3, det_lvl3 = oracle_cls.classify_expected_user_error(
            {"error": skill_3412_lvl3_err}, ui_state=trained_ui_state
        )
        self.assertTrue(ok_lvl3)
        self.assertEqual(det_lvl3["skill_id"], 3412)
        self.assertEqual(det_lvl3["required_level"], 3)
        self.assertEqual(det_lvl3["trained_level"], 1)

        # 2. Fail closed when ui_state is absent, empty, contradicts trained level, or claims non-skill reason.
        self.assertFalse(oracle_cls.is_expected_user_error({"error": skill_3412_err}))
        self.assertFalse(
            oracle_cls.is_expected_user_error({"error": skill_3412_err}, ui_state={})
        )
        self.assertFalse(
            oracle_cls.is_expected_user_error(
                {"error": skill_3412_err}, ui_state=empty_skills_ui_state
            )
        )
        # Pilot already trained skill 3412 to 1 & 3416 to 2 -> server rejection is a real bug!
        self.assertFalse(
            oracle_cls.is_expected_user_error(
                {"error": skill_3412_err}, ui_state=trained_ui_state
            )
        )
        self.assertFalse(
            oracle_cls.is_expected_user_error(
                {"error": skill_3416_err}, ui_state=trained_ui_state
            )
        )
        # Client sees trained=0 while server reports (trained: 1) -> skill desync must not be suppressed.
        self.assertFalse(
            oracle_cls.is_expected_user_error(
                {"error": skill_3412_lvl3_err}, ui_state=untrained_ui_state
            )
        )

        # Never blanket-exempt CPU/PG/slot/hardpoint/generic ModuleFitFailed even with populated ui_state.
        never_exempt_errors = (
            "UserError: User error, msg=ModuleFitFailed, dict={'moduleName': u'1MN Afterburner I', 'reason': u'Not enough CPU'}",
            'UserError("ModuleFitFailed", {"moduleName": "Damage Control I", "reason": "Max group fitted"})',
            "UserError('ModuleFitFailed', {'reason': 'Insufficient turret hardpoints on Badger: 1 fitted, hull limit is 1', 'moduleName': 'Miner I'})",
            "UserError('ModuleFitFailed', {'reason': 'Not enough powergrid', 'moduleName': '1MN Afterburner I'})",
            "UserError('ModuleFitFailed', {'reason': 'Missing required skill 3416'})",
            "UserError('ModuleFitFailed', {'reason': 'Low CPU', 'moduleName': ''})",
            "UserError('ModuleFitFailed', {'reason': 'Low CPU', 'moduleName': u'   '})",
            "UserError('ModuleFitFailed', {'reason': 'Low CPU', 'moduleName': None})",
            "UserError('ModuleFitFailed', {'moduleName': 'Small Shield Booster I'})",
            "UserError('ModuleFitFailed', {'moduleName': 'Small Shield Booster I', 'reason': ''})",
            "UserError('ModuleFitFailed', {})",
        )
        for err in never_exempt_errors:
            self.assertFalse(
                oracle_cls.is_expected_user_error(
                    {"error": err},
                    expected_text="Fitting rejects module or shows error modal",
                    ui_state=untrained_ui_state,
                ),
                err,
            )

        # 3. Fitting tasklet traceback with ModuleFitFailed exempts ONLY when live skill state verifies it.
        fitting_fit_failed_tb = (
            "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x9, ctxt='fittingSlotController.OnDropData'>\",) kwargs={}\n"
            'Traceback (most recent call last):\n'
            '  File "packages/bluepy/__init__.py", line 55, in CallWrapper\n'
            '  File "eve/client/script/ui/shared/fitting/fittingSlotController.py", line 310, in OnDropData\n'
            '  File "eve/client/script/inc/invCache.py", line 915, in TryFit\n'
            "UserError: User error, msg=ModuleFitFailed, dict={'reason': u'Missing required skill 3412 level 1 (trained: 0) for module Core Probe Launcher I', 'moduleName': u'Core Probe Launcher I'}\n"
        )
        self.assertFalse(
            oracle_cls._is_unhandled_tasklet_traceback(
                "UserError: User error, msg=ModuleFitFailed",
                fitting_fit_failed_tb,
                ui_state=untrained_ui_state,
            )
        )
        self.assertTrue(
            oracle_cls.is_ignorable_client_traceback(
                "UserError: User error, msg=ModuleFitFailed",
                fitting_fit_failed_tb,
                ui_state=untrained_ui_state,
            )
        )
        # Same traceback without ui_state or when pilot has skill 3412 -> must remain unhandled & flagged.
        self.assertTrue(
            oracle_cls._is_unhandled_tasklet_traceback(
                "UserError: User error, msg=ModuleFitFailed", fitting_fit_failed_tb
            )
        )
        self.assertFalse(
            oracle_cls.is_ignorable_client_traceback(
                "UserError: User error, msg=ModuleFitFailed", fitting_fit_failed_tb
            )
        )
        self.assertTrue(
            oracle_cls._is_unhandled_tasklet_traceback(
                "UserError: User error, msg=ModuleFitFailed",
                fitting_fit_failed_tb,
                ui_state=trained_ui_state,
            )
        )
        self.assertFalse(
            oracle_cls.is_ignorable_client_traceback(
                "UserError: User error, msg=ModuleFitFailed",
                fitting_fit_failed_tb,
                ui_state=trained_ui_state,
            )
        )

        missing_name_fitting_tb = fitting_fit_failed_tb.replace(
            ", 'moduleName': u'Core Probe Launcher I'", ""
        )
        self.assertTrue(
            oracle_cls._is_unhandled_tasklet_traceback(
                "UserError: User error, msg=ModuleFitFailed",
                missing_name_fitting_tb,
                ui_state=untrained_ui_state,
            )
        )
        self.assertFalse(
            oracle_cls.is_ignorable_client_traceback(
                "UserError: User error, msg=ModuleFitFailed",
                missing_name_fitting_tb,
                ui_state=untrained_ui_state,
            )
        )

    def test_tasklet_expected_user_error_exemptions_and_evaluate_step_clean(self):
        oracle_cls = playtester.PlaytesterBugOracle

        target_not_locked_tb = (
            "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0xb22ca3718, abps=1001, ctxt='<NO CONTEXT>^<bound method ModuleButton.OnStateChange of <ModuleButton object>>'>\",) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "packages/bluepy/__init__.py", line 55, in CallWrapper\n'
            '  File "eve/client/script/ui/inflight/shipModuleButton/shipmodulebutton.py", line 1630, in OnStateChange\n'
            '  File "eve/client/script/ui/inflight/shipModuleButton/shipmodulebutton.py", line 1580, in ActivateEffect\n'
            '  File "eve/client/script/environment/godma.py", line 2441, in Activate\n'
            "UserError: User error, msg=TargetNotLocked, dict={}\n"
        )
        radial_stop_ship_warp_tb = (
            "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x8f130f858, abps=1001, ctxt='RadialMenu::ClickButton:OnButtonClick'>\",) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "packages/bluepy/__init__.py", line 55, in CallWrapper\n'
            '  File "eve/client/script/ui/shared/radialMenu/radialMenuActions.py", line 264, in OnButtonClick\n'
            '  File "eve/client/script/ui/services/menuSvcExtras/movementFunctions.py", line 683, in StopMyShip\n'
            '  File "eve/client/script/ui/eveCommands.py", line 1150, in CmdStopShip\n'
            "UserError: User error, msg=ShipInWarp, dict={}\n"
        )
        window_blocked_tb = (
            "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x956b40158, abps=1001, ctxt='<NO CONTEXT>^<bound method EveCommandService.__OpenCargoHoldOfActiveShip_thread of <EveCommandService>>'>\",) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "packages/bluepy/__init__.py", line 55, in CallWrapper\n'
            '  File "eve/client/script/ui/eveCommands.py", line 1980, in __OpenCargoHoldOfActiveShip_thread\n'
            '  File "eve/client/script/ui/shared/inventory/invWindow.py", line 1357, in OpenOrShow\n'
            '  File "packages/carbonui/control/window.py", line 4164, in Open\n'
            "UserError: User error, msg=WindowBlockedFromOpening, dict={'window_name': u'Active ship'}\n"
        )
        autopilot_ship_in_warp_tb = (
            "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0xca370d558, abps=1001, ctxt=None>\",) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "autopilot.py", line 628, in __NavigateSystemTo\n'
            '  File "structureDocking.py", line 68, in RequestDocking\n'
            "UserError: User error, msg=ShipInWarp, dict={}\n"
        )

        for label, hl, tb in (
            ("TargetNotLocked", "UserError: User error, msg=TargetNotLocked, dict={}", target_not_locked_tb),
            ("ShipInWarp radial", "UserError: User error, msg=ShipInWarp, dict={}", radial_stop_ship_warp_tb),
            ("WindowBlockedFromOpening", "UserError: User error, msg=WindowBlockedFromOpening, dict={'window_name': u'Active ship'}", window_blocked_tb),
        ):
            self.assertFalse(oracle_cls._is_unhandled_tasklet_traceback(hl, tb), label)
            self.assertTrue(oracle_cls.is_ignorable_client_traceback(hl, tb), label)

        # Regression guard: autopilot tasklet ShipInWarp must remain unhandled/non-ignorable.
        self.assertTrue(
            oracle_cls._is_unhandled_tasklet_traceback(
                "UserError: User error, msg=ShipInWarp, dict={}", autopilot_ship_in_warp_tb
            )
        )
        self.assertFalse(
            oracle_cls.is_ignorable_client_traceback(
                "UserError: User error, msg=ShipInWarp, dict={}", autopilot_ship_in_warp_tb
            )
        )

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            qdir = root / "queue"
            tb_log = root / "client_tracebacks.log"
            srv = root / "server.log"
            shot = root / "screen.png"
            sdir = root / "step_98362"
            qdir.mkdir()
            sdir.mkdir()
            tb_log.write_text("", encoding="utf-8")
            srv.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")

            mem = playtester.PlaytesterMemory(root, git_head="496975a8e")
            oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)

            fr_clean = {
                "log_exceptions": [
                    {"headline": "UserError: User error, msg=TargetNotLocked, dict={}", "trace": target_not_locked_tb},
                    {"headline": "UserError: User error, msg=ShipInWarp, dict={}", "trace": radial_stop_ship_warp_tb},
                    {"headline": "UserError: User error, msg=WindowBlockedFromOpening, dict={'window_name': u'Active ship'}", "trace": window_blocked_tb},
                ],
                "quasar_errors": [],
                "macho_exceptions": [
                    {
                        "service": "invbroker",
                        "method": "MultiAdd",
                        "error": "UserError('ModuleFitFailed', {'reason': 'Missing required skill 3412 level 1 (trained: 0) for module Core Probe Launcher I (type 17938)', 'moduleName': 'Core Probe Launcher I'})",
                    }
                ],
            }
            untrained_snap = {
                "skills_state": {"total_trained_skills": 1, "trained_skill_levels": {"3300": 4}}
            }
            queued_clean = oracle.evaluate_step(
                step_num=98362,
                git_head="496975a8e",
                decision={"action": {"type": "click_widget", "target": "context_menu_Fit to Active Ship"}, "bug_report": {"should_file": False}},
                exec_res={"action_ok": True, "flight_recorder": fr_clean},
                ui_snapshot=untrained_snap,
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb_log,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(queued_clean, [])
            self.assertEqual(len(oracle.last_user_error_exemptions), 4)
            self.assertEqual(
                oracle._exemption_tracker.summary()["user_error_exemptions_count"], 4
            )
            self.assertEqual(
                oracle._exemption_tracker.summary()["precondition_verified_user_error_exemptions"],
                1,
            )

            # Empty moduleName on ModuleFitFailed must still file protocol_error (#1568/#1569 guard).
            queued_empty = oracle.evaluate_step(
                step_num=98363,
                git_head="496975a8e",
                decision={"action": {"type": "click_widget", "target": "context_menu_Fit to Active Ship"}, "bug_report": {"should_file": False}},
                exec_res={
                    "action_ok": False,
                    "flight_recorder": {
                        "log_exceptions": [],
                        "quasar_errors": [],
                        "macho_exceptions": [
                            {
                                "service": "invbroker",
                                "method": "MultiAdd",
                                "error": "UserError('ModuleFitFailed', {'reason': 'Missing required skill 3416'})",
                            }
                        ],
                    },
                },
                ui_snapshot={},
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb_log,
                server_log=srv,
                step_dir=sdir,
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
            )
            self.assertEqual(len(queued_empty), 1)
            self.assertEqual(
                json.loads(queued_empty[0].read_text(encoding="utf-8"))["kind"],
                "protocol_error",
            )

    def test_oracle_rejects_local_variables_and_non_user_error_crashes_for_exemptions(self):
        oracle_cls = playtester.PlaytesterBugOracle

        # 1. Real KeyError/AttributeError crashes inside exempted tasklets with UserError markers in Local variables / source lines.
        crash_cases = (
            (
                "fitting KeyError with ModuleFitFailed in locals",
                "[Playtester] Client exception: KeyError: 'shipID'",
                (
                    "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x1, ctxt='fittingSlotController.OnDropData'>\",) kwargs={}\n"
                    "Traceback (most recent call last):\n"
                    '  File "eve/client/script/ui/shared/fitting/fittingSlotController.py", line 310, in OnDropData\n'
                    "    raise UserError('ModuleFitFailed', {'moduleName': 'Core Probe Launcher I', 'reason': 'Missing skill'})\n"
                    "KeyError: 'shipID'\n"
                    "Local variables:\n"
                    "  err = \"UserError('ModuleFitFailed', {'moduleName': 'Core Probe Launcher I', 'reason': 'Missing skill'})\"\n"
                    "EXCEPTION END\n"
                ),
            ),
            (
                "ModuleButton AttributeError with TargetNotLocked in locals",
                "[Playtester] Client exception: AttributeError: 'NoneType'",
                (
                    "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x2, ctxt='ModuleButton.OnStateChange'>\",) kwargs={}\n"
                    "Traceback (most recent call last):\n"
                    '  File "eve/client/script/ui/inflight/shipModuleButton/shipmodulebutton.py", line 1630, in OnStateChange\n'
                    '  File "eve/client/script/ui/inflight/shipModuleButton/shipmodulebutton.py", line 1580, in ActivateEffect\n'
                    "AttributeError: 'NoneType' object has no attribute 'state'\n"
                    "Local variables:\n"
                    "  prev = 'UserError: User error, msg=TargetNotLocked, dict={}'\n"
                    "EXCEPTION END\n"
                ),
            ),
            (
                "RadialMenu TypeError with ShipInWarp in locals",
                "[Playtester] Client exception: TypeError: bad arg",
                (
                    "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x3, ctxt='RadialMenu::ClickButton:OnButtonClick'>\",) kwargs={}\n"
                    "Traceback (most recent call last):\n"
                    '  File "eve/client/script/ui/shared/radialMenu/radialMenuActions.py", line 264, in OnButtonClick\n'
                    '  File "eve/client/script/ui/eveCommands.py", line 1150, in CmdStopShip\n'
                    "TypeError: bad arg\n"
                    "Thread Locals:\n"
                    "  msg = 'ShipInWarp'\n"
                    "EXCEPTION END\n"
                ),
            ),
            (
                "Window.Open RuntimeError with WindowBlockedFromOpening in locals",
                "[Playtester] Client exception: RuntimeError: ui fail",
                (
                    "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x4, ctxt='Open'>\",) kwargs={}\n"
                    "Traceback (most recent call last):\n"
                    '  File "packages/carbonui/control/window.py", line 4164, in Open\n'
                    "RuntimeError: ui fail\n"
                    "Local variables:\n"
                    "  last = 'UserError: User error, msg=WindowBlockedFromOpening, dict={}'\n"
                    "EXCEPTION END\n"
                ),
            ),
        )
        for label, hl, tb in crash_cases:
            self.assertTrue(oracle_cls._is_unhandled_tasklet_traceback(hl, tb), label)
            self.assertFalse(oracle_cls.is_ignorable_client_traceback(hl, tb), label)
            self.assertFalse(oracle_cls.is_expected_user_error({"trace": tb, "headline": hl}), label)

        # 2. Missing-moduleName ModuleFitFailed with unrelated moduleName in Local variables or trailing sibling dict must NOT suppress (#1568/#1569).
        missing_name_with_locals = (
            "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x5, ctxt='fittingSlotController.OnDropData'>\",) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/fitting/fittingSlotController.py", line 310, in OnDropData\n'
            "    if not self.TryFit(item): pass\n"
            "UserError: User error, msg=ModuleFitFailed, dict={'reason': 'Low CPU'}\n"
            "Local variables:\n"
            "  item = {'moduleName': 'Core Probe Launcher I', 'reason': 'unused'}\n"
            "EXCEPTION END\n"
        )
        self.assertTrue(
            oracle_cls._is_unhandled_tasklet_traceback(
                "UserError: User error, msg=ModuleFitFailed, dict={'reason': 'Low CPU'}",
                missing_name_with_locals,
            )
        )
        self.assertFalse(
            oracle_cls.is_ignorable_client_traceback(
                "UserError: User error, msg=ModuleFitFailed, dict={'reason': 'Low CPU'}",
                missing_name_with_locals,
            )
        )
        self.assertFalse(
            oracle_cls.is_expected_user_error(
                {"error": "UserError('ModuleFitFailed', {'reason': 'Low CPU'}), meta={'moduleName': 'Core Probe Launcher I'}"}
            )
        )

        # 3. Concatenated multi-block trace combining KeyError + benign TargetNotLocked must remain non-ignorable.
        concat_crash_and_benign = (
            'Traceback (most recent call last):\n'
            '  File "eve/client/script/ui/shared/fitting/fittingSlotController.py", line 22, in OnDropData\n'
            "KeyError: 'boom'\n"
            'Traceback (most recent call last):\n'
            '  File "eve/client/script/environment/godma.py", line 2441, in Activate\n'
            "UserError: User error, msg=TargetNotLocked, dict={}\n"
        )
        self.assertFalse(oracle_cls.is_ignorable_client_traceback("", concat_crash_and_benign))
        self.assertFalse(oracle_cls.is_expected_user_error(concat_crash_and_benign))

        # 4. WindowBlockedFromOpening outside carbonui/control/window.py Open/ToggleOpenClose must remain unhandled.
        inv_open_or_show_tb = (
            "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x6, ctxt='OpenOrShow'>\",) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/shared/inventory/invWindow.py", line 1357, in OpenOrShow\n'
            "UserError: User error, msg=WindowBlockedFromOpening, dict={'window_name': u'Active ship'}\n"
        )
        self.assertTrue(
            oracle_cls._is_unhandled_tasklet_traceback(
                "UserError: User error, msg=WindowBlockedFromOpening", inv_open_or_show_tb
            )
        )

        # 5. Session-end flight recorder anomaly analyzer skips precondition-verified ModuleFitFailed,
        # flags CPU/unverified ModuleFitFailed and missing moduleName.
        untrained_ui_state = {
            "skills_state": {"total_trained_skills": 1, "trained_skill_levels": {"3300": 4}}
        }
        valid_calls = [
            {
                "service": "invbroker",
                "method": "MultiAdd",
                "seq": idx,
                "ts": 100.0 + idx,
                "status": "ERR(UserError('ModuleFitFailed', {'reason': 'Missing required skill 3412 level 1 (trained: 0) for module Core Probe Launcher I (type 17938)', 'moduleName': 'Core Probe Launcher I'}))",
                "error": "UserError('ModuleFitFailed', {'reason': 'Missing required skill 3412 level 1 (trained: 0) for module Core Probe Launcher I (type 17938)', 'moduleName': 'Core Probe Launcher I'})",
            }
            for idx in range(4)
        ]
        fr_exemptions = []
        self.assertEqual(
            oracle_cls.analyze_flight_recorder_anomalies(
                valid_calls, ui_state=untrained_ui_state, exemptions_out=fr_exemptions
            ),
            [],
        )
        self.assertEqual(len(fr_exemptions), 4)
        self.assertTrue(all(h["verified"] and h["skill_id"] == 3412 for h in fr_exemptions))

        # Without ui_state (or for CPU/PG/hardpoint), ModuleFitFailed must still be flagged as an anomaly.
        self.assertEqual(len(oracle_cls.analyze_flight_recorder_anomalies(valid_calls)), 1)
        cpu_calls = [
            {
                "service": "invbroker",
                "method": "MultiAdd",
                "seq": 50 + idx,
                "ts": 150.0 + idx,
                "status": "ERR(UserError('ModuleFitFailed', {'reason': 'Low CPU', 'moduleName': 'Miner I'}))",
                "error": "UserError('ModuleFitFailed', {'reason': 'Low CPU', 'moduleName': 'Miner I'})",
            }
            for idx in range(4)
        ]
        self.assertEqual(
            len(
                oracle_cls.analyze_flight_recorder_anomalies(
                    cpu_calls, ui_state=untrained_ui_state
                )
            ),
            1,
        )

        missing_name_calls = [
            {
                "service": "invbroker",
                "method": "MultiAdd",
                "seq": idx,
                "ts": 200.0 + idx,
                "status": "ERR(UserError('ModuleFitFailed', {'reason': 'Missing required skill 3412 level 1'}))",
                "error": "UserError('ModuleFitFailed', {'reason': 'Missing required skill 3412 level 1'})",
            }
            for idx in range(4)
        ]
        anoms = oracle_cls.analyze_flight_recorder_anomalies(missing_name_calls)
        self.assertEqual(len(anoms), 1)
        self.assertEqual(anoms[0]["kind"], "flight_recorder_rpc_anomaly")
        self.assertEqual(anoms[0]["non_ok_calls"], 4)

    def test_oracle_wrapper_callstack_isolation_and_flight_recorder_guard_order(self):
        oracle_cls = playtester.PlaytesterBugOracle

        # BF-USERERR-02: LogTraceback / LogException wrapper traces must never leak CALLSTACK: lines
        # containing benign markers or ModuleFitFailed dicts into downstream UserError checks.
        logtraceback_unexpected_tb = (
            'log.LogTraceback args=("UserError(\'UnexpectedServerBug\', {})",) kwargs={}\n'
            "None\n"
            "\n"
            "CALLSTACK:\n"
            '  File "eve/client/script/fitting.py", line 10, in Foo\n'
            "    # NotEnoughCPU\n"
            "    # UserError('ModuleFitFailed', {'reason': 'Missing skill', 'moduleName': 'Miner I'})\n"
        )
        ue_key, raised_txt = oracle_cls._extract_raised_user_error("", logtraceback_unexpected_tb)
        self.assertEqual(ue_key, "UnexpectedServerBug")
        self.assertNotIn("CALLSTACK:", raised_txt)
        self.assertNotIn("NotEnoughCPU", raised_txt)
        self.assertFalse(oracle_cls.is_expected_user_error({"trace": logtraceback_unexpected_tb, "headline": ""}))
        self.assertFalse(oracle_cls.is_ignorable_client_traceback("", logtraceback_unexpected_tb))

        logexception_wrapper_tb = (
            'logmodule.LogException args=("Unhandled exception in <TaskletExt object at 0x9, ctxt=\'fittingSlotController\'>",) kwargs={}\n'
            "None\n"
            "\n"
            "CALLSTACK:\n"
            '  File "eve/client/script/ui/shared/fittingScreen/fittingSlotController.py", line 50, in Add\n'
            "    # UserError('ModuleFitFailed', {'reason': 'Missing skill', 'moduleName': 'Miner I'})\n"
            "    # NotEnoughCPU\n"
        )
        self.assertFalse(oracle_cls.is_ignorable_client_traceback("", logexception_wrapper_tb))
        self.assertTrue(oracle_cls._is_unhandled_tasklet_traceback("", logexception_wrapper_tb))
        self.assertFalse(
            oracle_cls.is_ignorable_client_traceback(
                "UserError: User error, msg=UnexpectedServerBug, dict={}",
                logexception_wrapper_tb,
            )
        )

        # Positive control: LogTraceback directly carrying a state-verified ModuleFitFailed payload is expected.
        untrained_ui_state = {
            "skills_state": {"total_trained_skills": 1, "trained_skill_levels": {"3300": 4}}
        }
        logtraceback_valid_mff = (
            "log.LogTraceback args=(\"UserError('ModuleFitFailed', {'reason': 'Missing required skill 3412 level 1 (trained: 0)', 'moduleName': 'Miner I'})\",) kwargs={}\n"
            "None\n"
            "\n"
            "CALLSTACK:\n"
            '  File "eve/client/script/fitting.py", line 10, in Foo\n'
        )
        self.assertTrue(
            oracle_cls.is_expected_user_error(
                {"trace": logtraceback_valid_mff, "headline": ""},
                ui_state=untrained_ui_state,
            )
        )
        self.assertFalse(
            oracle_cls.is_expected_user_error(
                {"trace": logtraceback_valid_mff, "headline": ""}
            )
        )

        # BF-USERERR-R4-01 (BF-02): Empty-primary / truncated / CALLSTACK:-only traces must never
        # leak a UserError from CALLSTACK: via parse_traceback_summary fallback.
        mff_skill_line = (
            "UserError('ModuleFitFailed', {'reason': 'Missing required skill 3412 level 1 (trained: 0)', "
            "'moduleName': 'Miner I'})"
        )
        callstack_leak_cases = (
            "EXCEPTION #1\nNone\n\nCALLSTACK:\n  File \"foo.py\", line 10, in Bar\n"
            "    UserError: User error, msg=NotEnoughCPU, dict={}\n",
            "Traceback (most recent call last):\n"
            "  File \"foo.py\", line 10, in Bar\n\n"
            "CALLSTACK:\n"
            f"  Formatted exception info: {mff_skill_line}\n",
            "CALLSTACK:\n"
            "  File \"eve/client/script/fitting.py\", line 10, in Foo\n"
            f"  log.LogTraceback args=(\"{mff_skill_line}\",) kwargs={{}}\n",
        )
        for leak_tb in callstack_leak_cases:
            self.assertEqual(oracle_cls._extract_raised_user_error("", leak_tb), (None, ""))
            self.assertFalse(
                oracle_cls.is_expected_user_error(
                    {"trace": leak_tb, "headline": ""}, ui_state=untrained_ui_state
                )
            )
            self.assertFalse(
                oracle_cls.is_ignorable_client_traceback("", leak_tb, ui_state=untrained_ui_state)
            )

        # BF-USERERR-03: BENIGN_FLIGHT_FLOW_MARKERS must not short-circuit missing-moduleName ModuleFitFailed.
        for reason in (
            "FittingValidationFailed: missing skill",
            "NotEnoughCPU: 10 required",
        ):
            bad_mff_calls = [
                {
                    "service": "invbroker",
                    "method": "MultiAdd",
                    "seq": idx,
                    "ts": 300.0 + idx,
                    "status": f"ERR(UserError('ModuleFitFailed', {{'reason': '{reason}'}}))",
                    "error": f"UserError('ModuleFitFailed', {{'reason': '{reason}'}})",
                }
                for idx in range(4)
            ]
            anoms = oracle_cls.analyze_flight_recorder_anomalies(bad_mff_calls)
            self.assertEqual(len(anoms), 1, reason)
            self.assertEqual(anoms[0]["kind"], "flight_recorder_rpc_anomaly", reason)

        # P2 hardening: unexpected outer UserError or nested/string-embedded keys cannot spoof ModuleFitFailed;
        # Py2 long suffixes (483L) in sibling keys still validate top-level strings; CCP pre-frame open works.
        self.assertFalse(
            oracle_cls.is_expected_user_error(
                {
                    "error": (
                        "UserError('UnexpectedServerBug', {'detail': "
                        "\"UserError('ModuleFitFailed', {'reason': 'Low CPU', 'moduleName': 'Miner I'})\"})"
                    )
                }
            )
        )
        self.assertFalse(
            oracle_cls.is_expected_user_error(
                {
                    "error": (
                        "UserError('ModuleFitFailed', {'reason': \"Missing 'moduleName': 'Fake'\", "
                        "'nested': {'moduleName': 'Fake', 'reason': 'Fake'}})"
                    )
                }
            )
        )
        self.assertTrue(
            oracle_cls.is_expected_user_error(
                {
                    "error": (
                        "UserError('ModuleFitFailed', {'typeID': 483L, "
                        "'reason': 'Missing required skill 3412 level 1 (trained: 0) for Miner I', 'moduleName': 'Miner I'})"
                    )
                },
                ui_state=untrained_ui_state,
            )
        )
        self.assertFalse(
            oracle_cls.is_expected_user_error(
                {
                    "error": (
                        "UserError('ModuleFitFailed', {'typeID': 483L, "
                        "'reason': 'Insufficient turret hardpoints', 'moduleName': 'Miner I'})"
                    )
                },
                ui_state=untrained_ui_state,
            )
        )
        window_pre_tb = (
            "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x9, ctxt='Window.Open'>\",) kwargs={}\n"
            "  <pre>packages/carbonui/control/window.py(4164) Open</pre>\n"
            "UserError: User error, msg=WindowBlockedFromOpening, dict={}\n"
        )
        self.assertFalse(
            oracle_cls._is_unhandled_tasklet_traceback(
                "UserError: User error, msg=WindowBlockedFromOpening", window_pre_tb
            )
        )
        self.assertFalse(
            oracle_cls.is_expected_user_error(
                {
                    "error": (
                        "UserError('ModuleFitFailed', None) "
                        "('ModuleFitFailed', {'reason': 'x', 'moduleName': 'Miner I'})"
                    )
                }
            )
        )
        split_window_frames_tb = (
            "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x9, ctxt='Window.Close'>\",) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "packages/carbonui/control/window.py", line 100, in Close\n'
            '  File "eve/client/script/ui/shared/other.py", line 20, in Open\n'
            "UserError: User error, msg=WindowBlockedFromOpening, dict={}\n"
        )
        self.assertTrue(
            oracle_cls._is_unhandled_tasklet_traceback(
                "UserError: User error, msg=WindowBlockedFromOpening", split_window_frames_tb
            )
        )
        win_backslash_tb = window_pre_tb.replace("carbonui/control/window.py", "carbonui\\control\\window.py")
        self.assertFalse(
            oracle_cls._is_unhandled_tasklet_traceback(
                "UserError: User error, msg=WindowBlockedFromOpening", win_backslash_tb
            )
        )

        # BF-USERERR-R3-01: LogTraceback / LogException wrapper traces with benign UserError lines
        # inside CALLSTACK: or Local variables: must stay non-benign across _extract_tb_headline,
        # partition_log_exceptions, and evaluate_step.
        wrapper_callstack_and_locals_cases = (
            (
                "logtraceback_callstack_last",
                (
                    "log.LogTraceback args=('Unexpectedly got shipID = None',) kwargs={}\n"
                    "None\n\nCALLSTACK:\n"
                    '  File "eve/client/script/fitting.py", line 10, in Foo\n'
                    "    # UserError: User error, msg=NotEnoughCPU, dict={}\n"
                ),
            ),
            (
                "logexception_callstack_and_locals",
                (
                    'logmodule.LogException args=("Unhandled exception in <TaskletExt object at 0x9, ctxt=\'fittingSlotController\'>",) kwargs={}\n'
                    "None\n\nCALLSTACK:\n"
                    '  File "eve/client/script/ui/shared/fittingScreen/fittingSlotController.py", line 50, in Add\n'
                    "    # NotEnoughCPU\n"
                    "    # UserError('ModuleFitFailed', {'reason': 'Low CPU', 'moduleName': 'Miner I'})\n"
                    "\nLocal variables:\n"
                    "  prev = \"UserError: User error, msg=ModuleFitFailed, dict={'reason': 'Low CPU', 'moduleName': 'Miner I'}\"\n"
                ),
            ),
        )
        for idx, (label, wrapper_tb) in enumerate(wrapper_callstack_and_locals_cases):
            extracted_hl = oracle_cls._extract_tb_headline(wrapper_tb)
            self.assertNotIn("NotEnoughCPU", extracted_hl, label)
            self.assertNotIn("ModuleFitFailed", extracted_hl, label)

            non_benign, benign = oracle_cls.partition_log_exceptions([wrapper_tb])
            self.assertEqual(len(non_benign), 1, label)
            self.assertEqual(len(benign), 0, label)

            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                qdir = root / "queue"
                tb_log = root / "client_tracebacks.log"
                srv_log = root / "server.log"
                shot = root / "screen.png"
                sdir = root / f"step_{idx}"
                qdir.mkdir()
                sdir.mkdir()
                tb_log.write_text("", encoding="utf-8")
                srv_log.write_text("", encoding="utf-8")
                shot.write_bytes(b"png")
                mem = playtester.PlaytesterMemory(root, git_head="8900c28b7")
                oracle = playtester.PlaytesterBugOracle(root, qdir, mem, sync_github=False)
                queued = oracle.evaluate_step(
                    step_num=99000 + idx,
                    git_head="8900c28b7",
                    decision={"action": {"type": "wait", "target": "500"}, "bug_report": {"should_file": False}},
                    exec_res={
                        "action_ok": True,
                        "flight_recorder": {
                            "log_exceptions": [wrapper_tb],
                            "quasar_errors": [],
                            "macho_exceptions": [],
                        },
                    },
                    ui_snapshot={},
                    screenshot_path=shot,
                    burst_paths=[],
                    traceback_log=tb_log,
                    server_log=srv_log,
                    step_dir=sdir,
                )
                self.assertEqual(len(queued), 1, label)
                incident = json.loads(queued[0].read_text(encoding="utf-8"))
                self.assertEqual(incident["kind"], "client_traceback", label)

    def test_user_error_exemption_telemetry_visibility_and_dedup(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            mem_dir = root / "runtime"
            run_dir = root / "run_telemetry"
            step_dir = run_dir / "steps" / "step_00001"
            qdir = root / "queue"
            step_dir.mkdir(parents=True, exist_ok=True)
            qdir.mkdir(parents=True, exist_ok=True)
            tb_log = run_dir / "client_tracebacks.log"
            srv_log = run_dir / "server.log"
            shot = step_dir / "screen.png"
            tb_log.write_text("", encoding="utf-8")
            srv_log.write_text("", encoding="utf-8")
            shot.write_bytes(b"png")

            runner = playtester.PlaytesterRunner.__new__(playtester.PlaytesterRunner)
            runner.worktree_dir = root
            runner.runtime_dir = mem_dir
            runner.current_run_dir = run_dir
            runner.memory = playtester.PlaytesterMemory(mem_dir, git_head="496975a8e")
            runner._guard_tracker = playtester._GuardHitTracker(retain_all=False, max_recent=100)
            runner._sim_time_tracker = playtester._SimTimeTracker(retain_events=False, max_recent=200)
            runner._user_error_exemption_tracker = playtester._UserErrorExemptionTracker(
                retain_all=False, max_recent=100
            )
            runner.rpc = None
            oracle = playtester.PlaytesterBugOracle(root, qdir, runner.memory, sync_github=False)

            untrained_ui_state = {
                "skills_state": {"total_trained_skills": 1, "trained_skill_levels": {"3300": 4}}
            }
            mff_err = (
                "UserError('ModuleFitFailed', {'reason': 'Missing required skill 3412 level 1 "
                "(trained: 0) for module Core Probe Launcher I (type 17938)', "
                "'moduleName': 'Core Probe Launcher I'})"
            )
            window_tb = (
                "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x956b40158, "
                "ctxt='__OpenCargoHoldOfActiveShip_thread'>\",) kwargs={}\n"
                "Traceback (most recent call last):\n"
                '  File "packages/bluepy/__init__.py", line 55, in CallWrapper\n'
                '  File "packages/carbonui/control/window.py", line 4164, in Open\n'
                "UserError: User error, msg=WindowBlockedFromOpening, dict={'window_name': u'Active ship'}\n"
            )
            exec_res = {
                "action_ok": True,
                "flight_recorder": {
                    "macho_exceptions": [
                        {
                            "service": "invbroker",
                            "method": "MultiAdd",
                            "seq": 42,
                            "error": mff_err,
                        }
                    ],
                    "log_exceptions": [
                        {
                            "headline": "UserError: User error, msg=WindowBlockedFromOpening, dict={}",
                            "trace": window_tb,
                        }
                    ],
                    "quasar_errors": [],
                },
            }

            added, _ = runner._record_step_user_error_exemptions(
                1, exec_res, expected_text="", ui_state=untrained_ui_state
            )
            self.assertEqual(len(added), 2)
            # Re-recording or feeding oracle.evaluate_step / harvester on same seq=42 must not double-count.
            queued = oracle.evaluate_step(
                step_num=1,
                git_head="496975a8e",
                decision={"action": {"type": "click_widget", "target": "fit"}, "bug_report": {"should_file": False}},
                exec_res=exec_res,
                ui_snapshot=untrained_ui_state,
                screenshot_path=shot,
                burst_paths=[],
                traceback_log=tb_log,
                server_log=srv_log,
                step_dir=step_dir,
            )
            self.assertEqual(queued, [])
            runner._record_step_user_error_exemptions(
                1,
                exec_res,
                expected_text="",
                ui_state=untrained_ui_state,
                extra_hits=oracle.last_user_error_exemptions,
            )
            oracle.record_flight_recorder_anomalies(
                [
                    {
                        "service": "invbroker",
                        "method": "MultiAdd",
                        "seq": 42,
                        "ts": 100.0,
                        "status": f"ERR({mff_err})",
                        "error": mff_err,
                    }
                ],
                step_num=1,
                ui_state=untrained_ui_state,
                exemption_tracker=runner._ensure_user_error_exemption_state(),
            )
            runner.memory.apply_agent_decision(
                1,
                {"action": {"type": "click_widget", "target": "fit"}, "observation": "ok"},
                exec_res,
            )
            runner._write_run_summary(step_num=1, final=True)

            self.assertEqual(exec_res["user_error_exemption_count"], 2)
            self.assertEqual(
                exec_res["user_error_exemptions_by_key"],
                {"ModuleFitFailed": 1, "WindowBlockedFromOpening": 1},
            )
            run_sum = json.loads((run_dir / "run_summary.json").read_text(encoding="utf-8"))
            self.assertEqual(run_sum["user_error_exemptions_count"], 2)
            self.assertEqual(
                run_sum["user_error_exemptions_by_key"],
                {"ModuleFitFailed": 1, "WindowBlockedFromOpening": 1},
            )
            self.assertEqual(run_sum["precondition_verified_user_error_exemptions"], 1)
            self.assertEqual(run_sum["unverified_user_error_exemptions"], 1)
            self.assertEqual(len(run_sum["recent_user_error_exemptions"]), 2)

            mem_disk = json.loads((mem_dir / "memory.json").read_text(encoding="utf-8"))
            self.assertEqual(mem_disk["recent_actions"][-1]["user_error_exemption_count"], 2)
            self.assertEqual(mem_disk["last_run_summary"]["user_error_exemptions_count"], 2)
            self.assertEqual(
                mem_disk["last_run_summary"]["precondition_verified_user_error_exemptions"],
                1,
            )

            # Standalone macho_calls ERR (not mirrored in macho_exceptions) is also collected, while
            # mirrored seq=42 macho_calls ERR does not double-count.
            extra_calls_exec = {
                "flight_recorder": {
                    "macho_exceptions": [
                        {
                            "service": "invbroker",
                            "method": "MultiAdd",
                            "seq": 42,
                            "error": mff_err,
                        }
                    ],
                    "macho_calls": [
                        {
                            "service": "invbroker",
                            "method": "MultiAdd",
                            "seq": 42,
                            "status": "ERR",
                            "error": mff_err,
                        },
                        {
                            "service": "invbroker",
                            "method": "Add",
                            "seq": 99,
                            "status": "ERR",
                            "error": mff_err,
                        },
                    ],
                    "log_exceptions": [],
                }
            }
            calls_collected = playtester.PlaytesterBugOracle.collect_exec_res_user_error_exemptions(
                extra_calls_exec, ui_state=untrained_ui_state, step_num=3
            )
            self.assertEqual(
                [(h["channel"], h.get("seq")) for h in calls_collected],
                [("macho_exceptions", 42), ("macho_calls", 99)],
            )

            # Non-seq harvester anomalies preserve step_num (instead of step=0) and dedup across harvests.
            non_seq_calls = [
                {
                    "service": "invbroker",
                    "method": "MultiAdd",
                    "ts": 200.0 + i,
                    "status": f"ERR({mff_err})",
                    "error": mff_err,
                }
                for i in range(4)
            ]
            fresh_mem = playtester.PlaytesterMemory(root / "mem2", git_head="496975a8e")
            fresh_oracle = playtester.PlaytesterBugOracle(root, qdir, fresh_mem, sync_github=False)
            fresh_tracker = playtester._UserErrorExemptionTracker(retain_all=True, max_recent=100)
            self.assertEqual(
                fresh_oracle.record_flight_recorder_anomalies(
                    non_seq_calls,
                    step_num=7,
                    ui_state=untrained_ui_state,
                    exemption_tracker=fresh_tracker,
                ),
                [],
            )
            self.assertEqual(len(fresh_tracker.hits), 4)
            self.assertTrue(all(h["step"] == 7 for h in fresh_tracker.hits))
            self.assertTrue(all(str(h.get("dedup_id", "")).startswith("step:7:") for h in fresh_tracker.hits))
            # Second harvest of the exact same non-seq list must add zero duplicates.
            self.assertEqual(
                fresh_oracle.record_flight_recorder_anomalies(
                    non_seq_calls,
                    step_num=7,
                    ui_state=untrained_ui_state,
                    exemption_tracker=fresh_tracker,
                ),
                [],
            )
            self.assertEqual(len(fresh_tracker.hits), 4)
            self.assertEqual(fresh_tracker.summary()["user_error_exemptions_count"], 4)

    def test_scenario_checks_log_scan_and_runner_user_error_exemptions(self):
        untrained_ui_state = {
            "skills_state": {"total_trained_skills": 1, "trained_skill_levels": {"3300": 4}}
        }
        trained_ui_state = {
            "skills_state": {"total_trained_skills": 2, "trained_skill_levels": {"3300": 4, "3412": 2}}
        }
        mff_err = (
            "UserError('ModuleFitFailed', {'reason': 'Missing required skill 3412 level 1 "
            "(trained: 0) for module Core Probe Launcher I (type 17938)', "
            "'moduleName': 'Core Probe Launcher I'})"
        )
        cpu_err = "UserError('ModuleFitFailed', {'reason': 'Not enough CPU', 'moduleName': 'Miner I'})"
        window_tb = (
            "logmodule.LogException args=(\"Unhandled exception in <TaskletExt object at 0x956b40158, "
            "ctxt='__OpenCargoHoldOfActiveShip_thread'>\",) kwargs={}\n"
            "Traceback (most recent call last):\n"
            '  File "packages/bluepy/__init__.py", line 55, in CallWrapper\n'
            '  File "packages/carbonui/control/window.py", line 4164, in Open\n'
            "UserError: User error, msg=WindowBlockedFromOpening, dict={'window_name': u'Active ship'}\n"
        )

        # 1. evaluate_scenario_checks: exempts verified skill MFF (macho_exceptions & macho_calls) + tasklet TB.
        ok_pass, chk_pass, msg_pass = playtester.evaluate_scenario_checks(
            {"no_error_reply": True, "no_client_exception": True},
            untrained_ui_state,
            {},
            {
                "action_ok": True,
                "flight_recorder": {
                    "macho_exceptions": [
                        {"service": "invbroker", "method": "MultiAdd", "seq": 10, "error": mff_err}
                    ],
                    "macho_calls": [
                        {"service": "invbroker", "method": "Add", "seq": 11, "status": "ERR", "error": mff_err}
                    ],
                    "log_exceptions": [
                        {"headline": "UserError: User error, msg=WindowBlockedFromOpening, dict={}", "trace": window_tb}
                    ],
                },
            },
        )
        self.assertTrue(ok_pass, f"{chk_pass}: {msg_pass}")

        # Fail closed when skills_state shows trained skill or CPU reason.
        ok_trained, chk_trained, _ = playtester.evaluate_scenario_checks(
            {"no_error_reply": True},
            trained_ui_state,
            trained_ui_state,
            {
                "action_ok": True,
                "flight_recorder": {
                    "macho_calls": [
                        {"service": "invbroker", "method": "Add", "seq": 12, "status": "ERR", "error": mff_err}
                    ]
                },
            },
        )
        self.assertFalse(ok_trained)
        self.assertEqual(chk_trained, "no_error_reply")

        ok_cpu, chk_cpu, _ = playtester.evaluate_scenario_checks(
            {"no_error_reply": True},
            untrained_ui_state,
            untrained_ui_state,
            {
                "action_ok": True,
                "flight_recorder": {
                    "macho_exceptions": [
                        {"service": "invbroker", "method": "Add", "seq": 13, "error": cpu_err}
                    ]
                },
            },
        )
        self.assertFalse(ok_cpu)
        self.assertEqual(chk_cpu, "no_error_reply")

        # 2. scan_scenario_logs_for_errors forwards ui_state and populates exemptions_out.
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            srv_log = root / "server.log"
            cli_log = root / "client.log"
            tb_log = root / "client_tracebacks.log"
            srv_log.write_text("", encoding="utf-8")
            cli_log.write_text("", encoding="utf-8")
            mff_tb = (
                "EXCEPTION #1 logged at 10/11/26 20:00:00 : Client fitting blocked\n"
                "Traceback (most recent call last):\n"
                '  File "eve/client/script/ui/shared/fitting/fittingSlotController.py", line 185, in FitModule\n'
                f"UserError: User error, msg=ModuleFitFailed, dict={{'reason': 'Missing required skill 3412 level 1 (trained: 0)', 'moduleName': 'Core Probe Launcher I'}}\n"
                "EXCEPTION END\n"
            )
            tb_log.write_text(mff_tb + "\n" + window_tb + "\nEXCEPTION END\n", encoding="utf-8")

            # Without live skills_state -> MFF traceback fails closed.
            errs_no_ui = playtester.scan_scenario_logs_for_errors(
                srv_log, [cli_log], traceback_log_path=tb_log, ui_state={}, step_num=1
            )
            self.assertEqual(len(errs_no_ui), 1)
            self.assertIn("ModuleFitFailed", errs_no_ui[0])

            # With live untrained skills_state -> clean + both exemptions recorded.
            scan_ex = []
            errs_ok = playtester.scan_scenario_logs_for_errors(
                srv_log,
                [cli_log],
                traceback_log_path=tb_log,
                ui_state=untrained_ui_state,
                exemptions_out=scan_ex,
                step_num=2,
            )
            self.assertEqual(errs_ok, [])
            self.assertEqual(
                {h["key"] for h in scan_ex},
                {"ModuleFitFailed", "WindowBlockedFromOpening"},
            )
            self.assertEqual(sum(1 for h in scan_ex if h["verified"]), 1)

            # 3. ScenarioRunner end-to-end rollup & empty-post_state tail skill preservation.
            scen_path = root / "scen.json"
            scen_path.write_text(
                json.dumps(
                    {
                        "id": "t_scen_exemption_rollup",
                        "title": "Scenario exemption rollup",
                        "starting_state": {},
                        "steps": [
                            {"id": "step_1", "action": {"type": "wait", "wait_ms": 10}, "checks": {}},
                            {"id": "step_2", "action": {"type": "wait", "wait_ms": 10}, "checks": {}},
                        ],
                    }
                ),
                encoding="utf-8",
            )
            args = playtester.parse_args(["--scenario", str(scen_path), "--no-github-sync"])
            args.runtime_dir = str(root / "runtime")
            args.out_dir = str(root / "out")
            runner = playtester.ScenarioRunner(args, scenario_paths=[scen_path], out_dir=root / "out")
            runner.start_server = mock.MagicMock(return_value=True)
            runner.stop_server = mock.MagicMock()

            def _mock_start_client():
                scen_dir = runner.current_run_dir
                (scen_dir / "server.log").write_text("", encoding="utf-8")
                (scen_dir / "client.log").write_text("", encoding="utf-8")
                (scen_dir / "client_tracebacks.log").write_text("", encoding="utf-8")
                runner._last_boot_result = {
                    "state": {"session": {"charid": 90000001}, **untrained_ui_state},
                    "flight_recorder": {
                        "macho_calls": [
                            {"seq": 1, "service": "publicGatewaySvc", "method": "SendRequest", "status": "OK"}
                        ]
                    },
                }
                return True

            runner.start_client = mock.MagicMock(side_effect=_mock_start_client)
            runner._close_gm_bridge = mock.MagicMock()
            runner._capture_failure_screenshot = mock.MagicMock(return_value=None)
            runner._run_destiny_parity_check = mock.MagicMock(return_value=None)

            call_idx = {"n": 0}

            def _rpc_call(method, *_args, **_kwargs):
                if method == "get_state":
                    return {"state": {"session": {"charid": 90000001}, **untrained_ui_state}}
                if method == "execute_action":
                    call_idx["n"] += 1
                    if call_idx["n"] == 1:
                        return {
                            "action_ok": True,
                            "post_state": {"session": {"charid": 90000001}, **untrained_ui_state},
                            "flight_recorder": {
                                "macho_exceptions": [
                                    {"service": "invbroker", "method": "MultiAdd", "seq": 11, "error": mff_err}
                                ],
                                "macho_calls": [
                                    {"service": "invbroker", "method": "MultiAdd", "seq": 11, "status": "ERR", "error": mff_err}
                                ],
                                "log_exceptions": [
                                    {"headline": "UserError: User error, msg=WindowBlockedFromOpening, dict={}", "trace": window_tb}
                                ],
                            },
                        }
                    # Step 2 returns empty post_state while writing MFF traceback before tail scan.
                    return {"action_ok": True, "post_state": {}, "flight_recorder": {"macho_calls": []}}
                return {}

            def _stop_client():
                tb_file = runner.current_run_dir / "client_tracebacks.log"
                tb_file.write_text(mff_tb, encoding="utf-8")

            runner.stop_client = mock.MagicMock(side_effect=_stop_client)
            rpc_mock = mock.MagicMock()
            rpc_mock.call.side_effect = _rpc_call
            runner.rpc = rpc_mock

            rc = runner.run_scenarios()
            self.assertEqual(rc, 0)
            result = json.loads((root / "out" / "result.json").read_text(encoding="utf-8"))
            self.assertEqual(result["log_errors"], [])
            self.assertEqual(result["user_error_exemptions_count"], 3)
            self.assertEqual(
                result["user_error_exemptions_by_key"],
                {"ModuleFitFailed": 2, "WindowBlockedFromOpening": 1},
            )
            self.assertEqual(result["precondition_verified_user_error_exemptions"], 2)
            self.assertEqual(result["unverified_user_error_exemptions"], 1)
            j_entry = result["journeys"]["t_scen_exemption_rollup"]
            self.assertTrue(j_entry["passed"])
            self.assertEqual(j_entry["user_error_exemptions_count"], 3)
            self.assertEqual(j_entry["steps"][0]["user_error_exemption_count"], 2)


    def test_scenario_mirrored_evomu_site_tracebacks_and_tail_scan_dedup(self):
        tb_missing_skill = (
            "Action tasklet exception (click_widget):\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/fitting/fittingWnd.py", line 88, in FitModule\n'
            "UserError: ('ModuleFitFailed', {'moduleName': 'Light Ion Blaster I', "
            "'reason': 'Missing required skill 3300 level 1 (trained: 0)'})\n"
        )
        tb_target_not_locked = (
            "log.LogException Traceback:\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/inflight/shipHud/moduleButton.py", line 42, in ModuleButton.OnStateChange\n'
            '  File "eve/client/script/ui/inflight/shipHud/moduleButton.py", line 99, in ActivateEffect\n'
            "UserError: ('TargetNotLocked', {})\n"
        )
        tb_target_other_site = (
            "log.LogException Traceback:\n"
            "Traceback (most recent call last):\n"
            '  File "eve/client/script/ui/inflight/shipHud/moduleButton.py", line 55, in ModuleButton.OnStateChange\n'
            '  File "eve/client/script/ui/inflight/shipHud/weaponGroup.py", line 99, in ActivateEffect\n'
            "UserError: ('TargetNotLocked', {})\n"
        )
        live_ui_state = {
            "session": {"charid": 90000001},
            "skills_state": {"trained_skill_levels": {"3300": 0}},
        }

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            srv_log = root / "server.log"
            cli_log = root / "client.log"
            tb_log = root / "client_tracebacks.log"
            srv_log.write_text("", encoding="utf-8")

            # BF-USERERR-R6-01 (a): client.log [evomu_site] mirror + client_tracebacks.log + fr["log_exceptions"] -> 1
            cli_log.write_text(f"[evomu_site] {tb_missing_skill}", encoding="utf-8")
            tb_log.write_text(tb_missing_skill, encoding="utf-8")
            offsets: dict = {}
            log_hits: list = []
            errs = playtester.scan_scenario_logs_for_errors(
                srv_log,
                [cli_log],
                traceback_log_path=tb_log,
                log_offsets=offsets,
                ui_state=live_ui_state,
                exemptions_out=log_hits,
                step_num=1,
            )
            self.assertEqual(errs, [])
            self.assertEqual(len(log_hits), 1)
            self.assertEqual(log_hits[0]["channel"], "scenario_client_tracebacks")

            exec_res = {
                "flight_recorder": {
                    "log_exceptions": [
                        {
                            "seq": 1,
                            "headline": "UserError: ('ModuleFitFailed', {'moduleName': 'Light Ion Blaster I'})",
                            "trace": tb_missing_skill,
                        }
                    ]
                }
            }
            fr_hits = playtester.PlaytesterBugOracle.collect_exec_res_user_error_exemptions(
                exec_res,
                ui_state=live_ui_state,
                step_num=1,
                unseen_tracebacks=[{"headline": "", "trace": tb_missing_skill}],
            )
            self.assertEqual(len(fr_hits), 1)
            tracker = playtester._UserErrorExemptionTracker(retain_all=True, max_recent=50)
            absorbed = tracker.absorb_hits(fr_hits + log_hits, step_num=1)
            self.assertEqual(len(absorbed), 1)
            self.assertEqual(len(tracker.hits), 1)
            self.assertEqual(tracker.summary()["user_error_exemptions_count"], 1)

            # BF-USERERR-R6-01 (b): 2 genuine identical exceptions in step 2 mirrored across all 3 sinks -> 2 (not 1, not 6)
            cli_log.write_text(
                f"[evomu_site] {tb_missing_skill}" * 2,
                encoding="utf-8",
            )
            tb_log.write_text(tb_missing_skill * 2, encoding="utf-8")
            offsets_repeat: dict = {}
            repeat_log_hits: list = []
            self.assertEqual(
                playtester.scan_scenario_logs_for_errors(
                    srv_log,
                    [cli_log],
                    traceback_log_path=tb_log,
                    log_offsets=offsets_repeat,
                    ui_state=live_ui_state,
                    exemptions_out=repeat_log_hits,
                    step_num=2,
                ),
                [],
            )
            self.assertEqual(len(repeat_log_hits), 2)
            repeat_fr_hits = playtester.PlaytesterBugOracle.collect_exec_res_user_error_exemptions(
                {
                    "flight_recorder": {
                        "log_exceptions": [
                            {"seq": 1, "trace": tb_missing_skill},
                            {"seq": 2, "trace": tb_missing_skill},
                        ]
                    }
                },
                ui_state=live_ui_state,
                step_num=2,
            )
            repeat_tracker = playtester._UserErrorExemptionTracker(retain_all=True, max_recent=50)
            self.assertEqual(len(repeat_tracker.absorb_hits(repeat_fr_hits + repeat_log_hits, step_num=2)), 2)

            # BF-USERERR-R6-01 (c): 2 distinct callsites with same key stay distinct -> 2
            cli_log.write_text(
                f"[evomu_site] {tb_target_not_locked}[evomu_site] {tb_target_other_site}",
                encoding="utf-8",
            )
            tb_log.write_text(tb_target_not_locked + tb_target_other_site, encoding="utf-8")
            distinct_hits: list = []
            self.assertEqual(
                playtester.scan_scenario_logs_for_errors(
                    srv_log,
                    [cli_log],
                    traceback_log_path=tb_log,
                    log_offsets={},
                    exemptions_out=distinct_hits,
                    step_num=3,
                ),
                [],
            )
            self.assertEqual(len(distinct_hits), 2)

            # BF-USERERR-R6-01 (d): standalone [evomu_site] in client.log without client_tracebacks.log -> 1
            cli_log.write_text(f"[evomu_site] {tb_missing_skill}", encoding="utf-8")
            standalone_hits: list = []
            self.assertEqual(
                playtester.scan_scenario_logs_for_errors(
                    srv_log,
                    [cli_log],
                    traceback_log_path=root / "missing_tracebacks.log",
                    log_offsets={},
                    ui_state=live_ui_state,
                    exemptions_out=standalone_hits,
                    step_num=4,
                ),
                [],
            )
            self.assertEqual(len(standalone_hits), 1)
            self.assertEqual(standalone_hits[0]["channel"], "scenario_log:client.log")

        # P2 checks: invalid ModuleFitFailed bounds fail closed + evaluate_scenario_checks normalizes raw strings
        bad_lvl_err = (
            "UserError: ('ModuleFitFailed', {'moduleName': 'Light Ion Blaster I', "
            "'reason': 'Missing required skill 3300 level 6 (trained: 0)'})"
        )
        self.assertFalse(
            playtester.PlaytesterBugOracle.is_expected_user_error(bad_lvl_err, ui_state=live_ui_state)
        )
        bad_sid_err = (
            "UserError: ('ModuleFitFailed', {'moduleName': 'Light Ion Blaster I', "
            "'reason': 'Missing required skill 0 level 1 (trained: 0)'})"
        )
        self.assertFalse(
            playtester.PlaytesterBugOracle.is_expected_user_error(bad_sid_err, ui_state=live_ui_state)
        )
        ok_raw, chk_raw, reason_raw = playtester.evaluate_scenario_checks(
            {"no_client_exception": True},
            live_ui_state,
            live_ui_state,
            {
                "action_ok": True,
                "flight_recorder": {
                    "log_exceptions": [
                        f"{tb_missing_skill}\nCALLSTACK:\n  File \"fake.py\", line 1"
                    ]
                },
            },
            context={},
        )
        self.assertTrue(ok_raw, (chk_raw, reason_raw))
        ok_bad_raw, chk_bad_raw, reason_bad_raw = playtester.evaluate_scenario_checks(
            {"no_client_exception": True},
            live_ui_state,
            live_ui_state,
            {
                "action_ok": True,
                "flight_recorder": {
                    "log_exceptions": [
                        'Traceback (most recent call last):\n  File "foo.py", line 1\nKeyError: "x"'
                    ]
                },
            },
            context={},
        )
        self.assertFalse(ok_bad_raw)
        self.assertEqual(chk_bad_raw, "no_client_exception")
        self.assertIn("KeyError", reason_bad_raw)

        # BF-USERERR-R6-01 & BF-USERERR-R6-02 end-to-end ScenarioRunner.run_scenarios()
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen_path = root / "scen_r6.json"
            scen_path.write_text(
                json.dumps(
                    {
                        "id": "scen_r6_dedup_tail",
                        "title": "R6 mirror dedup and tail flush",
                        "starting_state": {"account": "demo", "char_id": 90000001},
                        "steps": [
                            {
                                "id": "step_mirror",
                                "action": {"type": "click_widget", "target": "fit_btn"},
                                "checks": {"no_error_reply": True, "no_client_exception": True},
                            },
                            {
                                "id": "step_tail_pre",
                                "action": {"type": "click_widget", "target": "lock_btn"},
                                "checks": {"no_error_reply": True, "no_client_exception": True},
                            },
                        ],
                    }
                ),
                encoding="utf-8",
            )
            args = types.SimpleNamespace(
                worktree=str(root),
                staged_app=str(root / "Missing.app"),
                macho_port=26000,
                bridge_port=26001,
                http_port=26002,
                gateway_port=26003,
                xmpp_port=26004,
                rpc_port=26005,
                model_chain="gemini-3-flash",
                model=None,
                hybrid=False,
                planner_model_chain=None,
                executor_model_chain=None,
                window_size="full",
                ui_scale=0.0,
                screenshot_max_dim=1024,
                persona=None,
                no_github_sync=True,
                scenarios=[str(scen_path)],
                out_dir=str(root / "out"),
                silent=True,
            )
            with mock.patch.object(playtester, "get_git_head", return_value="deadbeef"), \
                 mock.patch.object(playtester.PlaytesterRunner, "_server_bin_mtime", return_value=0.0):
                runner = playtester.ScenarioRunner(
                    args, scenario_paths=[scen_path], out_dir=root / "out"
                )

            def _fake_start_server(reset_db=False):
                runner.current_run_dir.mkdir(parents=True, exist_ok=True)
                (runner.current_run_dir / "server.log").write_text("", encoding="utf-8")
                return True

            def _fake_start_client():
                (runner.current_run_dir / "client_logs").mkdir(parents=True, exist_ok=True)
                (runner.current_run_dir / "client.log").write_text("", encoding="utf-8")
                (runner.current_run_dir / "client_logs" / "client_tracebacks.log").write_text(
                    "", encoding="utf-8"
                )
                return True

            def _fake_stop_client():
                # Flush a post-step traceback (same signature & key as final step's pre-stop flush!)
                (runner.current_run_dir / "client_logs").mkdir(parents=True, exist_ok=True)
                with (runner.current_run_dir / "client.log").open("a", encoding="utf-8") as f:
                    f.write(f"[evomu_site] {tb_missing_skill}")
                with (runner.current_run_dir / "client_logs" / "client_tracebacks.log").open(
                    "a", encoding="utf-8"
                ) as f:
                    f.write(tb_missing_skill)

            exec_idx = {"n": 0}

            def _fake_rpc_call(method, params=None, timeout=15.0):
                if method == "get_ui_state":
                    return dict(live_ui_state)
                if method == "execute_action":
                    exec_idx["n"] += 1
                    cli_p = runner.current_run_dir / "client.log"
                    tb_p = runner.current_run_dir / "client_logs" / "client_tracebacks.log"
                    if exec_idx["n"] == 1:
                        # Step 1: same exception across fr["log_exceptions"], client_tracebacks.log, [evomu_site] client.log -> 1
                        with cli_p.open("a", encoding="utf-8") as f:
                            f.write(f"[evomu_site] {tb_missing_skill}")
                        with tb_p.open("a", encoding="utf-8") as f:
                            f.write(tb_missing_skill)
                        return {
                            "action_ok": True,
                            "post_state": dict(live_ui_state),
                            "flight_recorder": {
                                "log_exceptions": [
                                    {"seq": 1, "headline": "UserError: ('ModuleFitFailed', {})", "trace": tb_missing_skill}
                                ]
                            },
                        }
                    # Step 2 (final step): pre-shutdown client_tracebacks.log + [evomu_site] client.log -> 1
                    with cli_p.open("a", encoding="utf-8") as f:
                        f.write(f"[evomu_site] {tb_missing_skill}")
                    with tb_p.open("a", encoding="utf-8") as f:
                        f.write(tb_missing_skill)
                    return {"action_ok": True, "post_state": {}, "flight_recorder": {}}
                return {}

            with mock.patch.object(runner, "start_server", side_effect=_fake_start_server), \
                 mock.patch.object(runner, "stop_server", return_value=None), \
                 mock.patch.object(runner, "start_client", side_effect=_fake_start_client), \
                 mock.patch.object(runner, "stop_client", side_effect=_fake_stop_client), \
                 mock.patch.object(runner, "_close_gm_bridge", return_value=None), \
                 mock.patch.object(runner, "_drain_host_gm_sim_events", return_value=None), \
                 mock.patch.object(runner.rpc, "call", side_effect=_fake_rpc_call):
                rc = runner.run_scenarios()

            self.assertEqual(rc, 0)
            payload = json.loads((root / "out" / "result.json").read_text(encoding="utf-8"))
            entry = payload["journeys"]["scen_r6_dedup_tail"]
            self.assertTrue(entry["passed"])
            # Step 1: 3-way mirror collapsed to 1; Step 2: 2-way log mirror collapsed to 1
            self.assertEqual(entry["steps"][0]["user_error_exemption_count"], 1)
            self.assertEqual(entry["steps"][1]["user_error_exemption_count"], 1)
            # Post-shutdown tail flush at new byte offset (with 2-way mirror collapsed to 1) preserved -> +1 = 3 total
            self.assertEqual(payload["user_error_exemptions_count"], 3)
            self.assertEqual(payload["precondition_verified_user_error_exemptions"], 3)
            self.assertEqual(len(entry["user_error_exemptions"]), 3)
            tail_hits = [
                h for h in entry["user_error_exemptions"]
                if str(h.get("channel") or "").startswith("scenario_tail_")
            ]
            self.assertEqual(len(tail_hits), 1)
            self.assertEqual(tail_hits[0]["channel"], "scenario_tail_client_tracebacks")


class TestVirtualClockDowntimeGuards(unittest.TestCase):
    def _seed_world_downtime(self, db_path: Path, *, offset_secs: int, downtime_until_unix: int) -> None:
        import sqlite3

        db_path.parent.mkdir(parents=True, exist_ok=True)
        conn = sqlite3.connect(str(db_path))
        try:
            conn.execute("CREATE TABLE world_meta (meta_key TEXT PRIMARY KEY, meta_value INTEGER NOT NULL)")
            conn.execute(
                "CREATE TABLE domain_rows (domain TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY (domain, key))"
            )
            conn.execute("INSERT INTO world_meta (meta_key, meta_value) VALUES ('clock_offset_secs', ?)", (int(offset_secs),))
            conn.execute(
                "INSERT INTO domain_rows (domain, key, value) VALUES (?, ?, ?)",
                (
                    "service_domains.cluster_and_runtime",
                    "cluster_downtime_state",
                    json.dumps({"downtime_until_unix": int(downtime_until_unix)}),
                ),
            )
            conn.commit()
        finally:
            conn.close()

    def test_virtual_clock_downtime_detection_and_wait_from_world_sqlite(self):
        # Virtual clock = wall + 31218 => wall 1795313984 (02:19:44Z) maps to virtual 11:00:02Z.
        offset_secs = 31218
        downtime_until = 1795346100
        wall_inside = 1795313984.0
        wall_after = 1795314920.0

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            staged = root / "staged_eve"
            staged.mkdir(parents=True)
            args = playtester.parse_args(["--no-github-sync", "--continuous", "--no-hybrid"])
            args.worktree = str(root)
            args.staged_app = str(staged)
            runner = playtester.PlaytesterRunner(args)
            try:
                self._seed_world_downtime(
                    runner.world_db_path,
                    offset_secs=offset_secs,
                    downtime_until_unix=downtime_until,
                )
                # Healthy step must remain False even while virtual downtime is active.
                self.assertFalse(
                    runner._is_cluster_downtime_active(
                        exec_res={"action_ok": True, "flight_recorder": {"macho_calls": [{"status": "OK"}]}},
                        ui_snapshot={"ui": {"modals": []}},
                        now_unix=wall_inside,
                    )
                )
                # Step #1584/#1585 disconnect artifacts under virtual downtime must return True.
                self.assertTrue(
                    runner._is_cluster_downtime_active(
                        exec_res={
                            "action_ok": False,
                            "action_error": "RuntimeError: Caller not in solsys nor station",
                            "flight_recorder": {
                                "log_exceptions": [{"headline": "ObjectCallGPCS.py, in PerformSessionCheck"}]
                            },
                        },
                        post_state={"ui": {"modals": [{"title": "Connection Lost"}]}},
                        now_unix=wall_inside,
                    )
                )
                # Once virtual clock passes downtime_until_unix & 11:15Z, disconnect must NOT be suppressed.
                self.assertFalse(
                    runner._is_cluster_downtime_active(
                        exec_res={
                            "action_ok": False,
                            "action_error": "RuntimeError: Caller not in solsys nor station",
                        },
                        post_state={"ui": {"modals": [{"title": "Connection Lost"}]}},
                        now_unix=wall_after,
                    )
                )

                # _wait_out_cluster_downtime must sleep ~903s (898 + 5s buffer), not fall back to 15s.
                fake_clock = {"t": wall_inside}
                slept = []

                def fake_time():
                    return fake_clock["t"]

                def fake_sleep(sec):
                    slept.append(sec)
                    fake_clock["t"] += sec

                waited = runner._wait_out_cluster_downtime(
                    reason="virtual downtime disconnect",
                    sleep_fn=fake_sleep,
                    time_fn=fake_time,
                    chunk_secs=300.0,
                )
                self.assertGreater(waited, 895.0)
                self.assertLess(waited, 910.0)
                self.assertAlmostEqual(sum(slept), 903.0, places=1)
            finally:
                runner.oracle.flush()

    def test_execute_step_virtual_downtime_disconnect_suppresses_bug_filing(self):
        offset_secs = 31218
        downtime_until = 1795346100
        wall_inside = 1795313984.0

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            staged = root / "staged_eve"
            staged.mkdir(parents=True)
            args = playtester.parse_args(["--no-github-sync", "--continuous"])
            args.worktree = str(root)
            args.staged_app = str(staged)
            runner = playtester.PlaytesterRunner(args)
            try:
                runner.current_run_dir.mkdir(parents=True, exist_ok=True)
                self._seed_world_downtime(
                    runner.world_db_path,
                    offset_secs=offset_secs,
                    downtime_until_unix=downtime_until,
                )
                pre_snap = {
                    "active_view": "hangar",
                    "session": {"charid": 90000001, "stationid": 60003760},
                    "open_windows": [],
                    "modals": [],
                    "ui": {
                        "modals": [],
                        "open_windows": [],
                        "clickable_widgets": [
                            {"name": "ItemHangar", "path": "ItemHangar", "label": "Item Hangar", "x": 100, "y": 200, "w": 80, "h": 24}
                        ],
                    },
                }
                disconnect_exec = {
                    "ok": False,
                    "action_ok": False,
                    "action_error": "Action blocked while disconnected or disconnect modal active (SessionDisconnected(no_userid))",
                    "error": "Action blocked while disconnected or disconnect modal active (SessionDisconnected(no_userid))",
                    "post_state": {
                        "active_view": "hangar",
                        "session": {"charid": None, "stationid": None},
                        "open_windows": [],
                        "modals": [{"title": "Connection Lost"}],
                        "ui": {"modals": [{"title": "Connection Lost"}], "open_windows": [], "clickable_widgets": []},
                    },
                    "flight_recorder": {
                        "modals_opened": [{"title": "Connection Lost"}],
                        "log_exceptions": [],
                        "macho_exceptions": [],
                        "quasar_errors": [],
                        "scatters_observed": [],
                    },
                }

                rpc_calls = []

                def fake_rpc_call(method, params=None, timeout=15.0):
                    rpc_calls.append((method, params))
                    if method == "get_state":
                        return pre_snap
                    if method == "execute_action":
                        return disconnect_exec
                    return {}

                def fake_cap(dest_path, **kwargs):
                    Path(dest_path).write_bytes(b"\x89PNG\r\n\x1a\n")
                    return {"ok": True, "mode": "mock"}

                fake_time = mock.Mock(return_value=wall_inside)
                decision = {
                    "thought": "click hangar item",
                    "persona_check": "ok",
                    "delegated_plan": {
                        "batch_goal": "Click ItemHangar",
                        "routine_name": "click_hangar",
                        "max_batch_steps": 1,
                        "horizon_rationale": "Test disconnect bug suppression",
                        "success_criteria": "Attempt click",
                        "abort_conditions": [],
                        "steps": [
                            {
                                "step_number": 1,
                                "exec_mode": "direct",
                                "capture_screenshot": False,
                                "action_type_hint": "click_widget",
                                "target_or_command_hint": "ItemHangar",
                                "instruction": "Click ItemHangar",
                                "expected_outcome": "ItemHangar clicked",
                                "action": {"type": "click_widget", "target": "ItemHangar"},
                                "assertions": {},
                            }
                        ],
                    },
                }
                with mock.patch("playtester.time.time", fake_time), \
                     mock.patch("playtester.get_git_head", return_value="f249af816"), \
                     mock.patch("playtester.capture_window_screenshot", side_effect=fake_cap), \
                     mock.patch("playtester.invoke_agy_planner", return_value=(decision, "test-model", 0.1)), \
                     mock.patch("playtester.invoke_agy_executor", side_effect=AssertionError("unexpected invoke_agy_executor")) as m_exec, \
                     mock.patch("playtester.subprocess.run", side_effect=AssertionError("unexpected subprocess.run")) as m_subproc, \
                     mock.patch.object(runner, "_drain_host_gm_sim_events", return_value=None), \
                     mock.patch.object(runner.rpc, "call", side_effect=fake_rpc_call):
                    runner.execute_step(1, {}, [])
                runner.oracle.flush(timeout=5.0)

                m_exec.assert_not_called()
                m_subproc.assert_not_called()
                self.assertTrue(
                    any(
                        method == "execute_action"
                        and isinstance(params, dict)
                        and isinstance(params.get("action"), dict)
                        and params["action"].get("type") == "click_widget"
                        and params["action"].get("target") == "ItemHangar"
                        for method, params in rpc_calls
                    )
                )
                self.assertTrue(runner._downtime_pause_requested)
                self.assertEqual(list(runner.bug_queue_dir.glob("*.json")), [])
            finally:
                runner.oracle.flush()

    def test_read_world_downtime_state_survives_missing_world_meta(self):
        import sqlite3

        with tempfile.TemporaryDirectory() as td:
            db_path = Path(td) / "partial#db.sqlite"
            conn = sqlite3.connect(str(db_path))
            try:
                conn.execute(
                    "CREATE TABLE domain_rows (domain TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY (domain, key))"
                )
                conn.execute(
                    "INSERT INTO domain_rows (domain, key, value) VALUES (?, ?, ?)",
                    (
                        "service_domains.cluster_and_runtime",
                        "cluster_downtime_state",
                        json.dumps({"downtime_until_unix": 1795346100}),
                    ),
                )
                conn.commit()
            finally:
                conn.close()
            self.assertEqual(playtester._read_world_downtime_state(db_path), (0, 1795346100))

    def test_resolve_wait_action_ms_expands_warp_arrival_phrase_only_when_short(self):
        norm = playtester._normalize_action_defaults(
            {"type": "wait", "target": "wait for warp arrival", "wait_ms": 250}
        )
        self.assertEqual(norm["wait_ms"], 8000)
        self.assertEqual(
            playtester._resolve_wait_action_ms("wait", "warp_arrival", None, None),
            8000,
        )
        self.assertEqual(
            playtester._resolve_wait_action_ms("wait", "warp_arrival", None, 12000),
            12000,
        )

    def test_build_reasoning_prompt_surfaces_active_inventory_filter_warning(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            mem = playtester.PlaytesterMemory(root, git_head="e032a958d")
            shot = root / "screen.png"
            shot.write_bytes(b"png")
            prompt_filtered = playtester.build_reasoning_prompt(
                step_num=101,
                screenshot_path=shot,
                burst_paths=[],
                ui_snapshot={
                    "inventory_state": {
                        "cargo_item_count": 0,
                        "hangar_item_count": 0,
                        "quick_filter": "trit",
                        "active_filters": ["High Slots"],
                    }
                },
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                memory=mem,
                git_head="e032a958d",
            )
            self.assertIn("Active Inventory Filter Warning", prompt_filtered)
            self.assertIn("quick_filter='trit'", prompt_filtered)

            prompt_clear = playtester.build_reasoning_prompt(
                step_num=102,
                screenshot_path=shot,
                burst_paths=[],
                ui_snapshot={
                    "inventory_state": {
                        "cargo_item_count": 1,
                        "hangar_item_count": 2,
                        "quick_filter": None,
                        "active_filters": None,
                    }
                },
                last_flight_recorder={"log_exceptions": [], "quasar_errors": [], "macho_exceptions": []},
                memory=mem,
                git_head="e032a958d",
            )
            self.assertNotIn("Active Inventory Filter Warning", prompt_clear)

    def test_clear_and_warp_flags_preserved_in_normalization_direct_resolution_and_schemas(self):
        # 1) _normalize_action_defaults / _normalize_executor_decision preserve clear & negative warp flags
        exec_norm = playtester._normalize_executor_decision(
            {
                "action": {
                    "type": "type_text",
                    "target": "QuickFilterEdit",
                    "clear": True,
                    "expect_in_warp_rejection": True,
                    "allow_in_warp_error": True,
                }
            }
        )
        self.assertTrue(exec_norm["action"]["clear"])
        self.assertIsNone(exec_norm["action"]["command"])
        self.assertTrue(exec_norm["action"]["expect_in_warp_rejection"])
        self.assertTrue(exec_norm["action"]["allow_in_warp_error"])

        # 2) _normalize_planner_decision + _resolve_direct_action must preserve clear=True
        # WITHOUT falling back command = "QuickFilterEdit".
        plan_dec = playtester._normalize_planner_decision(
            {
                "delegated_plan": {
                    "plan_goal": "Clear filter",
                    "steps": [
                        {
                            "step_number": 1,
                            "exec_mode": "direct",
                            "action": {
                                "type": "type_text",
                                "target": "QuickFilterEdit",
                                "clear": True,
                            },
                        }
                    ],
                }
            }
        )
        step = plan_dec["delegated_plan"]["steps"][0]
        self.assertEqual(step["target_or_command_hint"], "QuickFilterEdit")
        resolved, reason = playtester._resolve_direct_action(
            step,
            ui_snapshot={
                "ui": {
                    "clickable_widgets": [
                        {"name": "quickFilterInputBox", "path": "QuickFilterEdit", "cls": "QuickFilterEdit"}
                    ]
                }
            },
        )
        self.assertEqual(reason, "")
        self.assertTrue(resolved["clear"])
        self.assertIsNone(resolved["command"])

        # 3) All three schemas allow clear, expect_in_warp_rejection, and allow_in_warp_error.
        for schema_name, prop_getter in (
            ("playtester_action_schema.json", lambda d: d["properties"]["action"]["properties"]),
            ("playtester_executor_schema.json", lambda d: d["properties"]["action"]["properties"]),
            (
                "playtester_planner_schema.json",
                lambda d: d["properties"]["delegated_plan"]["properties"]["steps"]["items"]["properties"]["action"]["properties"],
            ),
        ):
            props = prop_getter(json.loads((HARNESS_DIR / schema_name).read_text(encoding="utf-8")))
            for flag_key in ("clear", "expect_in_warp_rejection", "allow_in_warp_error"):
                self.assertIn(flag_key, props, f"{flag_key} missing in {schema_name}")
                self.assertEqual(props[flag_key]["type"], "boolean")

        # 4) Unified host RPC timeout helper covers space_command subtypes cleanly.
        self.assertEqual(playtester._compute_step_rpc_timeout({"type": "space_command", "target": "approach"}), 35.0)
        self.assertEqual(playtester._compute_step_rpc_timeout({"type": "approach"}), 35.0)
        self.assertEqual(playtester._compute_step_rpc_timeout({"type": "space_command", "target": "dock"}), 65.0)
        self.assertEqual(playtester._compute_step_rpc_timeout({"type": "space_command", "target": "jump_gate"}), 75.0)


if __name__ == "__main__":
    unittest.main()
