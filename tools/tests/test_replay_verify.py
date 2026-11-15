#!/usr/bin/env python3
"""Unit tests for tools/replay (VirtualClock, TraceRecorder, golden diff, and destiny invariants)."""

import contextlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest

TOOLS_DIR = Path(__file__).resolve().parents[1]
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))

from replay import (  # noqa: E402
    TraceRecorder,
    VirtualClock,
    check_destiny_invariants,
    compare_golden,
    extract_golden,
    load_trace,
    verify_trace_data,
)
from replay import verify_scenario, verify_trace  # noqa: E402


def _build_valid_navigation_trace() -> dict:
    clock = VirtualClock(initial_tick=5000, tick_ms=1000, tidi_factor=1.0)
    rec = TraceRecorder(
        "undock_approach_orbit_stop",
        clock=clock,
        entity_aliases={"200000001": "owner_ship", "200000002": "observer_ship", "40009077": "station"},
        sessions={"owner": {"char_id": 90000001}, "observer": {"char_id": 90000002}},
    )
    rec.record_rpc_in("owner", "beyonce", "MachoBindObject", args=[30000142])
    rec.record_destiny_update(
        "owner",
        epoch=1,
        stamp=5000,
        has_set_state=True,
        actions=[{"action": "SetState", "entities": [200000001, 40009077]}],
    )
    rec.record_destiny_update(
        "observer",
        epoch=1,
        stamp=5000,
        has_set_state=True,
        actions=[{"action": "SetState", "entities": [200000002, 40009077]}],
    )
    rec.record_clock_advance(1000)
    rec.record_destiny_update(
        "observer",
        epoch=1,
        stamp=5001,
        actions=[
            {
                "action": "AddBalls2",
                "entities": [200000001],
                "metadata": {"correlation_id": "undock-1"},
            }
        ],
    )
    rec.record_rpc_in("owner", "beyonce", "CmdGotoDirection", args=[1.0, 0.0, 0.0])
    rec.record_destiny_update(
        "owner",
        epoch=1,
        stamp=5001,
        actions=[
            {
                "action": "GotoDirection",
                "entities": [200000001],
                "metadata": {"correlation_id": "undock-1"},
            }
        ],
    )
    rec.record_clock_advance(1000)
    rec.record_rpc_in("owner", "beyonce", "CmdOrbit", args=[40009077, 5000.0])
    rec.record_destiny_update(
        "owner",
        epoch=1,
        stamp=5002,
        actions=[{"action": "Orbit", "entities": [200000001]}],
    )
    rec.record_clock_advance(1000)
    rec.record_rpc_in("owner", "beyonce", "CmdStop")
    rec.record_destiny_update(
        "owner",
        epoch=1,
        stamp=5003,
        actions=[{"action": "Stop", "entities": [200000001]}],
    )
    return rec.to_dict()


class ReplayVerifyTests(unittest.TestCase):
    def test_virtual_clock_small_steps_and_tidi_scaling(self) -> None:
        clock = VirtualClock(initial_tick=100, tick_ms=1000, tidi_factor=0.5)
        for _ in range(10):
            clock.advance(100)
        self.assertEqual(clock.sim_ms, 1000)
        self.assertEqual(clock.current_tick, 100)
        for _ in range(10):
            clock.advance(100)
        self.assertEqual(clock.sim_ms, 2000)
        self.assertEqual(clock.current_tick, 101)

        with self.assertRaises(ValueError):
            clock.advance(-1)
        with self.assertRaises(ValueError):
            clock.set_tidi(0.0)

    def test_extract_golden_and_update_golden_roundtrip(self) -> None:
        trace = _build_valid_navigation_trace()
        with tempfile.TemporaryDirectory() as tmp:
            tmp_dir = Path(tmp)
            trace_path = tmp_dir / "nav.trace.json"
            golden_path = tmp_dir / "nav.golden.json"

            rec = TraceRecorder("undock_approach_orbit_stop")
            # Save and reload the trace to verify serialization roundtrip.
            from replay.trace import save_trace

            save_trace(trace, trace_path)
            loaded = load_trace(trace_path)
            golden = extract_golden(loaded)

            self.assertEqual(golden["scenario"], "undock_approach_orbit_stop")
            self.assertEqual(
                [e["rel_stamp"] for e in golden["sessions"]["owner"]],
                [0, 1, 2, 3],
            )
            self.assertEqual(
                [e["action"] for e in golden["sessions"]["owner"]],
                ["SetState", "GotoDirection", "Orbit", "Stop"],
            )
            self.assertEqual(golden["sessions"]["owner"][1]["entities"], ["owner_ship"])

            # Missing golden fails without UPDATE_GOLDEN=1.
            ok_missing, msg_missing = compare_golden(golden, golden_path, update_golden=False)
            self.assertFalse(ok_missing)
            self.assertIn("missing golden file", msg_missing)

            # UPDATE_GOLDEN=1 via env var creates golden and passes.
            prev_env = os.environ.get("UPDATE_GOLDEN")
            try:
                os.environ["UPDATE_GOLDEN"] = "1"
                buf = io.StringIO()
                with contextlib.redirect_stdout(buf):
                    rc = verify_trace.main(["--trace", str(trace_path), "--golden", str(golden_path)])
                self.assertEqual(rc, 0, buf.getvalue())
                self.assertTrue(golden_path.is_file())
            finally:
                if prev_env is None:
                    os.environ.pop("UPDATE_GOLDEN", None)
                else:
                    os.environ["UPDATE_GOLDEN"] = prev_env

            # Subsequent verification without UPDATE_GOLDEN matches cleanly.
            buf2 = io.StringIO()
            with contextlib.redirect_stdout(buf2):
                rc2 = verify_trace.main(["--trace", str(trace_path), "--golden", str(golden_path)])
            self.assertEqual(rc2, 0, buf2.getvalue())

            # Mutate trace to trigger a golden diff mismatch.
            mutated = json.loads(json.dumps(loaded))
            mutated["events"][-1]["actions"][0]["action"] = "WarpTo"
            ok_diff, res_diff = verify_trace_data(mutated, golden_path, update_golden=False)
            self.assertFalse(ok_diff)
            self.assertFalse(res_diff["golden_ok"])
            self.assertIn("-        \"action\": \"Stop\"", res_diff["golden_message"])
            self.assertIn("+        \"action\": \"WarpTo\"", res_diff["golden_message"])
            self.assertIsNotNone(rec)

    def test_invariants_pass_for_valid_combat_warp_and_missile_traces(self) -> None:
        clock = VirtualClock(initial_tick=2000)
        rec = TraceRecorder("turret_kill_wreck_pod", clock=clock)
        rec.record_destiny_update(
            "owner",
            epoch=1,
            stamp=2000,
            has_set_state=True,
            actions=[{"action": "SetState", "entities": [100, 200]}],
        )
        rec.record_clock_advance(1000)
        rec.record_destiny_update(
            "owner",
            epoch=1,
            stamp=2001,
            actions=[
                {"action": "GotoDirection", "entities": [100]},
                {
                    "action": "OnSpecialFX",
                    "entities": [100],
                    "metadata": {"module_id": 1001, "guid": "effects.Laser"},
                },
                {
                    "action": "OnSpecialFX",
                    "entities": [100],
                    "metadata": {"module_id": 1002, "guid": "effects.Laser"},
                },
            ],
        )
        rec.record_clock_advance(1000)
        rec.record_destiny_update(
            "owner",
            epoch=1,
            stamp=2002,
            actions=[
                {"action": "AddBalls2", "entities": [500], "metadata": {"missile_owner": 100}},
                {"action": "TerminalPlayDestructionEffect", "entities": [200]},
            ],
        )
        rec.record_clock_advance(1000)
        rec.record_destiny_update(
            "owner",
            epoch=1,
            stamp=2003,
            actions=[
                {"action": "RemoveBalls", "entities": [200]},
                {"action": "AddBalls2", "entities": [900], "metadata": {"wreck_of": 200}},
                {"action": "OnDeactivatingWarp", "entities": [100]},
                {"action": "AddBalls2", "entities": [300], "metadata": {"phase": "warp_arrival"}},
            ],
        )
        violations = check_destiny_invariants(rec.to_dict())
        self.assertEqual(violations, [])

    def test_invariants_catch_every_violation_class(self) -> None:
        # 1. Stamp regression + clock mismatch
        t1 = {
            "schema_version": 1,
            "scenario": "bad_stamps",
            "virtual_clock": {"initial_tick": 10, "tick_ms": 1000, "tidi_factor": 1.0},
            "events": [
                {"kind": "clock_advance", "delta_ms": 1000, "sim_ms": 1000, "tick": 99},
                {
                    "kind": "out_frame",
                    "frame_type": "DoDestinyUpdate",
                    "session": "owner",
                    "epoch": 1,
                    "stamp": 11,
                    "sim_ms": 1000,
                    "actions": [{"action": "GotoDirection", "entities": [1]}],
                },
                {
                    "kind": "out_frame",
                    "frame_type": "DoDestinyUpdate",
                    "session": "owner",
                    "epoch": 1,
                    "stamp": 10,
                    "sim_ms": 900,
                    "actions": [{"action": "Stop", "entities": [1]}],
                },
            ],
        }
        v1 = check_destiny_invariants(t1)
        joined1 = "\n".join(v1)
        self.assertIn("tick mismatch", joined1)
        self.assertIn("sim_ms regressed", joined1)
        self.assertIn("non-monotonic DoDestinyUpdate stamp", joined1)

        # 2. Update before AddBalls2, duplicate in frame, and update after RemoveBalls
        t2 = {
            "schema_version": 1,
            "scenario": "bad_lifecycle",
            "events": [
                {
                    "kind": "out_frame",
                    "frame_type": "DoDestinyUpdate",
                    "session": "owner",
                    "epoch": 1,
                    "stamp": 10,
                    "actions": [{"action": "SetState", "entities": [1]}],
                },
                {
                    "kind": "out_frame",
                    "frame_type": "DoDestinyUpdate",
                    "session": "owner",
                    "epoch": 1,
                    "stamp": 11,
                    "actions": [
                        {"action": "GotoDirection", "entities": [2]},
                        {"action": "GotoDirection", "entities": [2]},
                    ],
                },
                {
                    "kind": "out_frame",
                    "frame_type": "DoDestinyUpdate",
                    "session": "owner",
                    "epoch": 1,
                    "stamp": 12,
                    "actions": [
                        {"action": "AddBalls2", "entities": [2]},
                        {"action": "RemoveBalls", "entities": [2]},
                    ],
                },
                {
                    "kind": "out_frame",
                    "frame_type": "DoDestinyUpdate",
                    "session": "owner",
                    "epoch": 1,
                    "stamp": 13,
                    "actions": [{"action": "Stop", "entities": [2]}],
                },
            ],
        }
        v2 = "\n".join(check_destiny_invariants(t2))
        self.assertIn("before AddBalls2", v2)
        self.assertIn("duplicate action GotoDirection", v2)
        self.assertIn("after RemoveBalls", v2)

        # 3. Death sequence order, warp arrival stamp, missile lag, owner/observer lead
        t3 = {
            "schema_version": 1,
            "scenario": "bad_causality",
            "events": [
                {
                    "kind": "out_frame",
                    "frame_type": "DoDestinyUpdate",
                    "session": "owner",
                    "epoch": 1,
                    "stamp": 100,
                    "actions": [
                        {"action": "GotoDirection", "entities": [10], "metadata": {"correlation_id": "c1"}},
                        {"action": "AddBalls2", "entities": [300], "metadata": {"wreck_of": 20}},
                        {"action": "RemoveBalls", "entities": [20]},
                        {"action": "TerminalPlayDestructionEffect", "entities": [20]},
                        {"action": "AddBalls2", "entities": [400], "metadata": {"phase": "warp_arrival"}},
                    ],
                },
                {
                    "kind": "out_frame",
                    "frame_type": "DoDestinyUpdate",
                    "session": "owner",
                    "epoch": 1,
                    "stamp": 105,
                    "actions": [
                        {"action": "AddBalls2", "entities": [500], "metadata": {"missile_owner": 10}},
                        {"action": "OnDeactivatingWarp", "entities": [10]},
                    ],
                },
                {
                    "kind": "out_frame",
                    "frame_type": "DoDestinyUpdate",
                    "session": "observer",
                    "epoch": 1,
                    "stamp": 90,
                    "actions": [
                        {"action": "AddBalls2", "entities": [10], "metadata": {"correlation_id": "c1"}},
                    ],
                },
            ],
        }
        # Note: in t3, wreck AddBalls2 preceded RemoveBalls, RemoveBalls preceded TerminalPlayDestructionEffect,
        # missile AddBalls2 at 105 > 100 + 2, owner c1 (100) > observer c1 (90) + 2.
        v3 = "\n".join(check_destiny_invariants(t3))
        self.assertIn("before victim RemoveBalls", v3)
        self.assertIn("TerminalPlayDestructionEffect after RemoveBalls", v3)
        self.assertIn("missile AddBalls2 stamp outside", v3)
        self.assertIn("observer stamp", v3)

        # Also verify destination AddBalls2 behind OnDeactivatingWarp stamp.
        t4 = {
            "schema_version": 1,
            "scenario": "bad_warp",
            "events": [
                {
                    "kind": "out_frame",
                    "frame_type": "DoDestinyUpdate",
                    "session": "owner",
                    "epoch": 1,
                    "stamp": 200,
                    "actions": [{"action": "OnDeactivatingWarp", "entities": [10]}],
                },
                {
                    "kind": "out_frame",
                    "frame_type": "DoDestinyUpdate",
                    "session": "owner",
                    "epoch": 1,
                    "stamp": 199,
                    "actions": [{"action": "AddBalls2", "entities": [300], "metadata": {"phase": "warp_arrival"}}],
                },
            ],
        }
        v4 = "\n".join(check_destiny_invariants(t4))
        self.assertIn("behind OnDeactivatingWarp", v4)

    def test_verify_scenario_cli_list_and_run(self) -> None:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = verify_scenario.main(["--list"])
        self.assertEqual(rc, 0)
        self.assertIn("undock_approach_orbit_stop", buf.getvalue())
        self.assertIn("gate_jump_tidi", buf.getvalue())

        trace = _build_valid_navigation_trace()
        with tempfile.TemporaryDirectory() as tmp:
            tmp_dir = Path(tmp)
            tpath = tmp_dir / "t.json"
            gpath = tmp_dir / "g.json"
            from replay.trace import save_trace

            save_trace(trace, tpath)
            buf_up = io.StringIO()
            with contextlib.redirect_stdout(buf_up):
                rc_up = verify_scenario.main(
                    [
                        "undock_approach_orbit_stop",
                        "--trace",
                        str(tpath),
                        "--golden",
                        str(gpath),
                        "--update-golden",
                    ]
                )
            self.assertEqual(rc_up, 0, buf_up.getvalue())

            buf_ok = io.StringIO()
            with contextlib.redirect_stdout(buf_ok):
                rc_ok = verify_scenario.main(
                    [
                        "undock_approach_orbit_stop",
                        "--trace",
                        str(tpath),
                        "--golden",
                        str(gpath),
                        "--json",
                    ]
                )
            self.assertEqual(rc_ok, 0)
            self.assertTrue(json.loads(buf_ok.getvalue())["passed"])


if __name__ == "__main__":
    unittest.main()
