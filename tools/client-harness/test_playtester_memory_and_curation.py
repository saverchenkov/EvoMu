#!/usr/bin/env python3
"""Offline unit tests for playtester personas, coverage frontier normalization, and memory curation."""

from __future__ import annotations

import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

HARNESS_DIR = Path(__file__).resolve().parent
if str(HARNESS_DIR) not in sys.path:
    sys.path.insert(0, str(HARNESS_DIR))

import curate_memory
import playtester


class TestPlaytesterPersonasAndFrontier(unittest.TestCase):
    def test_personas_rotate_steps_and_canonical_frontier(self):
        cfg = json.loads((HARNESS_DIR / "playtester_personas.json").read_text(encoding="utf-8"))
        personas = cfg["personas"]
        self.assertEqual(len(personas), 9)
        persona_ids = {p["id"] for p in personas}
        for p in personas:
            rot = int(p["rotate_after_steps"])
            self.assertGreaterEqual(rot, 45, f"{p['id']} rotate_after_steps too low: {rot}")
            self.assertLessEqual(rot, 60, f"{p['id']} rotate_after_steps too high: {rot}")
            self.assertGreaterEqual(len(p.get("goals") or []), 3, f"{p['id']} needs checkpoint goals")

        frontier = cfg["initial_frontier"]
        covered_personas = {f["persona"] for f in frontier}
        self.assertEqual(
            covered_personas,
            persona_ids,
            f"All 9 personas must have initial_frontier entries; missing={persona_ids - covered_personas}",
        )
        feature_ids = [f["feature_id"] for f in frontier]
        self.assertEqual(len(feature_ids), len(set(feature_ids)), "Duplicate feature_id in initial_frontier")
        self.assertEqual(len(feature_ids), 33)
        self.assertIn("space.nav.stargate_jump_roundtrip", feature_ids)
        self.assertIn("space.scanning.dscan_and_probes", feature_ids)
        self.assertIn("ui.chaos.keyboard_shortcuts_and_focus", feature_ids)
        self.assertIn("upwell.deploy.anchor_and_quantum_core", feature_ids)
        self.assertIn("titan.combat.doomsday_and_phenomena", feature_ids)


class TestPlaytesterMemoryCurationAndLatchFix(unittest.TestCase):
    def test_normalize_prunes_orphans_maps_aliases_and_decays_on_rebuild(self):
        with tempfile.TemporaryDirectory() as td:
            state_dir = Path(td)
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
                    for i in range(150)
                ],
            }
            (state_dir / "memory.json").write_text(json.dumps(raw_mem), encoding="utf-8")

            # Same git head & mtime: keep buggy flags as-is, but prune all 150 empty-persona noise entries
            mem = playtester.PlaytesterMemory(state_dir, git_head="aaa1111", server_bin_mtime=1000.0)
            by_feat = {x["feature"]: x for x in mem.data["coverage_frontier"]}
            self.assertEqual(by_feat["ui.skills.queue_and_training"]["status"], "verified")
            self.assertEqual(by_feat["ui.fitting.equip_online_and_undock_verify"]["status"], "verified")
            self.assertEqual(by_feat["space.mining.asteroid_lock_and_laser_cycle"]["status"], "buggy")
            self.assertEqual(by_feat["ui.market.search_buy_and_cancel"]["status"], "buggy")
            self.assertTrue(all(x.get("persona") for x in mem.data["coverage_frontier"]))
            self.assertEqual(len(mem.data["coverage_frontier"]), 33)

            # Server rebuild (new git head): auto-decay buggy -> untested + needs_retest=True
            stats = mem.normalize_coverage_frontier(git_head="bbb2222", server_bin_mtime=1050.0, save=True)
            self.assertTrue(stats["server_rebuilt"])
            self.assertEqual(stats["buggy_decayed"], 2)
            by_feat2 = {x["feature"]: x for x in mem.data["coverage_frontier"]}
            self.assertEqual(by_feat2["space.mining.asteroid_lock_and_laser_cycle"]["status"], "untested")
            self.assertTrue(by_feat2["space.mining.asteroid_lock_and_laser_cycle"]["needs_retest"])
            self.assertEqual(by_feat2["space.mining.asteroid_lock_and_laser_cycle"]["previously_buggy_at_step"], 120)
            self.assertEqual(mem.data["last_server_git_head"], "bbb2222")
            self.assertEqual(mem.data["last_server_bin_mtime"], 1050.0)

    def test_buggy_latch_fixed_and_custom_features_capped_on_repo_personas(self):
        with tempfile.TemporaryDirectory() as td:
            state_dir = Path(td)
            mem = playtester.PlaytesterMemory(state_dir, git_head="deadbeef")
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
            by_feat = {x["feature"]: x for x in mem.data["coverage_frontier"]}
            self.assertEqual(by_feat["space.mining.asteroid_lock_and_laser_cycle"]["status"], "buggy")

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

    def test_curate_memory_cli_dry_run_and_idempotency(self):
        with tempfile.TemporaryDirectory() as td:
            wt = Path(td)
            pdir = wt / ".playtester"
            pdir.mkdir(parents=True)
            mpath = pdir / "memory.json"
            mpath.write_text(
                json.dumps(
                    {
                        "total_steps": 9000,
                        "coverage_frontier": [
                            {"feature": "space.mining.venture_and_mining_hold", "persona": "mining_and_resource_harvesting", "status": "buggy", "buggy_at_step": 8400},
                            {"feature": "orphan_1", "persona": "", "status": "untested"},
                            {"feature": "orphan_2", "persona": "", "status": "verified"},
                        ],
                    }
                ),
                encoding="utf-8",
            )
            dry_stats = curate_memory.curate_memory_file(
                memory_path=mpath,
                worktree_dir=wt,
                decay_stale_buggy=True,
                dry_run=True,
            )
            self.assertTrue(dry_stats["dry_run"])
            self.assertEqual(dry_stats["buggy_decayed"], 1)
            self.assertEqual(dry_stats["empty_persona_count"], 0)
            # File unchanged during dry run
            self.assertEqual(len(json.loads(mpath.read_text(encoding="utf-8"))["coverage_frontier"]), 3)

            real_stats = curate_memory.curate_memory_file(
                memory_path=mpath,
                worktree_dir=wt,
                decay_stale_buggy=True,
                dry_run=False,
            )
            self.assertIn("backup_path", real_stats)
            self.assertTrue(Path(real_stats["backup_path"]).exists())
            curated = json.loads(mpath.read_text(encoding="utf-8"))
            self.assertEqual(len(curated["coverage_frontier"]), 33)

            # Second run should decay 0 and keep exact count 33
            second_stats = curate_memory.curate_memory_file(
                memory_path=mpath,
                worktree_dir=wt,
                decay_stale_buggy=False,
                dry_run=True,
            )
            self.assertEqual(second_stats["orphans_pruned"], 0)
            self.assertEqual(second_stats["after_count"], 33)

    def test_curate_memory_main_cli_flags_and_no_backup(self):
        with tempfile.TemporaryDirectory() as td:
            wt = Path(td)
            pdir = wt / ".playtester"
            pdir.mkdir(parents=True)
            mpath = pdir / "memory.json"
            mpath.write_text(
                json.dumps(
                    {
                        "total_steps": 120,
                        "coverage_frontier": [
                            {"feature": "ui.skills.queue_and_training", "persona": "regression_and_critical_ui", "status": "buggy", "buggy_at_step": 100},
                            {"feature": "orphan_noise", "persona": "", "status": "untested"},
                        ],
                    }
                ),
                encoding="utf-8",
            )
            fake_bin = wt / "target" / "debug" / "eve-server"
            fake_bin.parent.mkdir(parents=True)
            fake_bin.write_bytes(b"bin")

            buf = io.StringIO()
            with redirect_stdout(buf):
                rc = curate_memory.main(
                    [
                        "--worktree",
                        str(wt),
                        "--server-bin",
                        str(fake_bin),
                        "--decay-stale-buggy",
                        "--no-backup",
                    ]
                )
            self.assertEqual(rc, 0)
            stats = json.loads(buf.getvalue())
            self.assertEqual(stats["buggy_decayed"], 1)
            self.assertEqual(stats["orphans_pruned"], 1)
            self.assertEqual(stats["after_count"], 33)
            self.assertNotIn("backup_path", stats)
            self.assertEqual(list(pdir.glob("*.bak")), [])
            saved = json.loads(mpath.read_text(encoding="utf-8"))
            self.assertEqual(len(saved["coverage_frontier"]), 33)


if __name__ == "__main__":
    unittest.main()
