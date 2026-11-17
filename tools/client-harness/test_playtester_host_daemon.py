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
        self.assertEqual(playtester._normalize_action_defaults({"type": "type"})["type"], "type_text")


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
        admins_gm = parsed_gm.get("admin_accounts", [])
        self.assertGreaterEqual(len(admins_gm), 1)
        for a_acc in admins_gm:
            unknown_a = set(a_acc.keys()) - struct_fields["AdminAccountConfig"]
            self.assertEqual(unknown_a, set(), f"Unknown AdminAccountConfig keys: {unknown_a}")

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


if __name__ == "__main__":
    unittest.main()
