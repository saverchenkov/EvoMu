#!/usr/bin/env python3
"""Unit tests for tools/classify_client_log.py."""

import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest

TOOLS_DIR = Path(__file__).resolve().parents[1]
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))

import classify_client_log as ccl  # noqa: E402


CLEAN_V24_LOG = """\
2026-11-15 16:00:00.100 info michelle DoDestinyUpdate call for tick 1000 containing 2 updates.  waitForBubble= False
2026-11-15 16:00:00.101 info michelle Server Update with 2 event(s) added to history
2026-11-15 16:00:00.102 info michelle ------ History Dump 1000 -------
2026-11-15 16:00:00.102 info michelle state waiting: no
2026-11-15 16:00:00.102 info michelle [ 1000 ] SetState
2026-11-15 16:00:00.102 info michelle [ 1000 ] AddBalls2
2026-11-15 16:00:00.103 info michelle  
2026-11-15 16:00:00.105 info michelle Action:     SetState 1000 - current: None ({'ego': 200000001L},)
2026-11-15 16:00:00.106 info michelle Action:    AddBalls2 1000 - current: 1000 ((b'\\x01', [({'itemID': 200000001L, 'typeID': 587}, (1.0, 24000.0, 1000))]),)
2026-11-15 16:00:01.100 info michelle DoDestinyUpdate call for tick 1001 containing 1 updates.  waitForBubble= False
2026-11-15 16:00:01.105 info michelle Action: GotoDirectio 1001 - current: 1001 (200000001L, 1.0, 0.0, 0.0)
"""


STALE_ACTION_LOG = """\
DoDestinyUpdate call for tick 500 containing 2 updates.  waitForBubble= False
Action:    AddBalls2 498 - current: 500 ((b'\\x01', [({'itemID': 200000001L}, (1.0, 1.0, 498))]),)
Action:  OnSpecialFX 500 - current: 500 (200000001L, 300000010L, None, None, None, 'effects.Warping', False, 1, 1, -1, None, None, 0, None)
DoDestinyUpdate call for tick 501 containing 1 updates.  waitForBubble= False
Action:         Stop 499 - current: 501 (200000002,)
"""


STALE_HISTORY_LOG = """\
DoDestinyUpdate call for tick 700 containing 3 updates.  waitForBubble= False
------ Park History Dump 705 -------
state waiting: no
[ 702 ] AddBalls2
[ 703 ] GotoDirection (x 2)
[ 705 ] Stop
[ 701 ] RemoveBall

Action:    AddBalls2 702 - current: 702 ((b'\\x01', [({'itemID': 200000010}, (1.0, 1.0, 702))]),)
Action: GotoDirectio 703 - current: 703 (200000010, 0.0, 1.0, 0.0)
Action:         Stop 705 - current: 705 (200000010,)
"""


MALFORMED_LOG = """\
DoDestinyUpdate call for tick notanumber containing 1 updates.
------ History Dump badcur -------
------ History Dump 199 -------
[ notastamp ] AddBalls2
[ 199 ] AddBalls2
Action: AddBalls2 badstamp - current: 100
Action: AddBalls2 100 - current:
Action:
Found update batch with 2 items and 1 timestamps
Action: (100, ('AddBalls2', ()))
DoDestinyUpdate call for tick 200 containing 1 updates.  waitForBubble= False
------ History Dump 200 -------
state waiting: yes
[ 200 ] AddBalls2
Action:    AddBalls2 200 - current: 200 ((b'\\x01', [({'itemID': 200000099}, ())]),)
Michelle discarded a state that was too old 195 < 200
Failed to synchronize to 198 Requesting new state
"""


MULTI_ENTITY_LOG = """\
DoDestinyUpdate call for tick 900 containing 4 updates.  waitForBubble= False
Action:    AddBalls2 899 - current: 900 ((b'\\x01', [({'itemID': 200000001L}, ()), ({'itemID': 200000002L}, ())]),)
Action:        Orbit 900 - current: 900 (200000001L, 40009077L, 5000.0)
Action:   FollowBall 901 - current: 901 (200000002L, 200000001L, 2500.0)
Action:  RemoveBalls 901 - current: 902 ([200000002L, 200000003L],)
Action: OnAudioActiv 902 - current: 902 ()
"""


class ClassifyClientLogTests(unittest.TestCase):
    def _run_cli(self, log_text: str, extra_args: list[str]) -> tuple[int, str, str]:
        with tempfile.TemporaryDirectory() as tmp:
            log_path = Path(tmp) / "client.log"
            log_path.write_text(log_text, encoding="utf-8")
            stdout_buf = io.StringIO()
            stderr_buf = io.StringIO()
            with contextlib.redirect_stdout(stdout_buf), contextlib.redirect_stderr(stderr_buf):
                rc = ccl.main([str(log_path), *extra_args])
            return rc, stdout_buf.getvalue(), stderr_buf.getvalue()

    def test_clean_log_passes_parity_and_fails_stale(self) -> None:
        rc, out, err = self._run_cli(CLEAN_V24_LOG, ["--expect", "parity", "--json"])
        self.assertEqual(rc, 0, f"expected parity pass, got rc={rc} out={out} err={err}")
        data = json.loads(out)
        self.assertTrue(data["passed"])
        self.assertEqual(data["ticks_seen"], 2)
        self.assertEqual(data["first_tick"], 1000)
        self.assertEqual(data["last_tick"], 1001)
        self.assertEqual(data["actions_total"], 3)
        self.assertEqual(data["stale_at_action"], 0)
        self.assertEqual(data["stale_at_history"], 0)
        self.assertEqual(data["history_dumps_seen"], 1)
        self.assertEqual(data["history_entries_total"], 2)
        self.assertEqual(data["malformed_lines"], 0)
        ent = data["per_entity"]["200000001"]
        self.assertEqual(ent["actions"], 3)
        self.assertEqual(ent["stale_at_action"], 0)
        self.assertEqual(ent["actions_by_name"]["GotoDirection"], 1)

        rc_stale, out_stale, _ = self._run_cli(CLEAN_V24_LOG, ["--expect", "stale"])
        self.assertEqual(rc_stale, 1)
        self.assertIn("[FAIL]", out_stale)

    def test_stale_at_action_fails_parity_and_passes_stale(self) -> None:
        rc_parity, out_parity, _ = self._run_cli(STALE_ACTION_LOG, ["--expect", "parity", "--json"])
        self.assertEqual(rc_parity, 1)
        data = json.loads(out_parity)
        self.assertFalse(data["passed"])
        self.assertEqual(data["stale_at_action"], 2)
        self.assertEqual(data["actions_total"], 3)
        self.assertEqual(data["per_entity"]["200000001"]["stale_at_action"], 1)
        self.assertEqual(data["per_entity"]["200000002"]["stale_at_action"], 1)
        self.assertEqual(len(data["stale_action_events"]), 2)
        self.assertEqual(data["stale_action_events"][0]["delta"], -2)
        self.assertEqual(data["stale_action_events"][1]["delta"], -2)

        rc_stale, out_stale, _ = self._run_cli(STALE_ACTION_LOG, ["--expect", "stale", "-v"])
        self.assertEqual(rc_stale, 0)
        self.assertIn("[PASS]", out_stale)
        self.assertIn("Stale action events:", out_stale)

        rc_any, _, _ = self._run_cli(STALE_ACTION_LOG, ["--expect", "any"])
        self.assertEqual(rc_any, 0)

    def test_stale_at_history_counts_and_entity_correlation(self) -> None:
        rc, out, _ = self._run_cli(STALE_HISTORY_LOG, ["--expect", "parity", "--json"])
        # Actions themselves have stamp == current, so stale-at-action is 0 while stale-at-history is > 0.
        self.assertEqual(rc, 0)
        data = json.loads(out)
        self.assertEqual(data["stale_at_action"], 0)
        # [702] AddBalls2 (1) + [703] GotoDirection (x 2 -> 2) + [701] RemoveBall (1) = 4 stale history entries.
        self.assertEqual(data["stale_at_history"], 4)
        self.assertEqual(data["history_entries_total"], 5)
        self.assertEqual(data["per_entity"]["200000010"]["history_entries"], 4)
        self.assertEqual(data["per_entity"]["200000010"]["stale_at_history"], 3)
        self.assertEqual(data["per_entity"]["unknown"]["stale_at_history"], 1)

    def test_malformed_and_partial_lines_recover_cleanly(self) -> None:
        rc, out, _ = self._run_cli(MALFORMED_LOG, ["--expect", "parity", "--json"])
        self.assertEqual(rc, 0)
        data = json.loads(out)
        # 5 malformed lines: bad tick, bad History Dump header, [notastamp], badstamp Action, truncated current:, bare Action: -> 6!
        # Note "Found update batch..." + "Action: (100, ...)" is handled as a desync warning, not malformed.
        self.assertEqual(data["malformed_lines"], 6)
        self.assertEqual(data["ticks_seen"], 1)
        self.assertEqual(data["actions_total"], 1)
        self.assertEqual(data["discarded_old_states"], 1)
        self.assertEqual(data["desync_warnings"], 2)
        self.assertEqual(data["per_entity"]["200000099"]["actions"], 1)

    def test_multi_entity_tracking_and_l_suffix(self) -> None:
        rc, out, _ = self._run_cli(MULTI_ENTITY_LOG, ["--expect", "any", "--json"])
        self.assertEqual(rc, 0)
        data = json.loads(out)
        self.assertEqual(data["stale_at_action"], 2)
        self.assertEqual(data["per_entity"]["200000001"]["actions"], 2)
        self.assertEqual(data["per_entity"]["200000001"]["stale_at_action"], 1)
        self.assertEqual(data["per_entity"]["200000002"]["actions"], 3)
        self.assertEqual(data["per_entity"]["200000002"]["stale_at_action"], 2)
        self.assertEqual(data["per_entity"]["200000003"]["actions"], 1)
        self.assertEqual(data["per_entity"]["200000003"]["stale_at_action"], 1)
        self.assertEqual(data["per_entity"]["unknown"]["actions"], 1)
        self.assertEqual(data["per_entity"]["unknown"]["actions_by_name"]["OnAudioActivated"], 1)

    def test_empty_log_and_stdin_and_missing_file(self) -> None:
        rc_empty, out_empty, _ = self._run_cli("", ["--expect", "parity"])
        self.assertEqual(rc_empty, 1)
        self.assertIn("no destiny ticks", out_empty)

        rc_allowed, _, _ = self._run_cli("", ["--expect", "parity", "--allow-empty"])
        self.assertEqual(rc_allowed, 0)

        report = ccl.classify_logs(["-"], stdin=io.StringIO(CLEAN_V24_LOG))
        self.assertEqual(report.actions_total, 3)
        self.assertEqual(report.stale_at_action, 0)

        stderr_buf = io.StringIO()
        with contextlib.redirect_stderr(stderr_buf):
            rc_missing = ccl.main(["/nonexistent/path/to/client.log"])
        self.assertEqual(rc_missing, 2)


if __name__ == "__main__":
    unittest.main()
