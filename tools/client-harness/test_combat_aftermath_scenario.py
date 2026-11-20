#!/usr/bin/env python3
"""
Unit tests for `scenarios/combat_aftermath.json` assertions (TT-13 / TT-14 / #1206).
Verifies static scenario validation plus positive and negative mutation evaluations
across loot-all, salvaging, platinum insurance debit, zero SCC self-destruct payout
parity, combat log RPCs, and kill report window population.
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

HARNESS_DIR = Path(__file__).resolve().parent
if str(HARNESS_DIR) not in sys.path:
    sys.path.insert(0, str(HARNESS_DIR))

import playtester  # noqa: E402


SCENARIO_PATH = HARNESS_DIR / "scenarios" / "combat_aftermath.json"


def _load_steps_by_id():
    data = json.loads(SCENARIO_PATH.read_text(encoding="utf-8"))
    return data, {step["id"]: step for step in data.get("steps", [])}


def _exec_ok(calls=None, scatters=None):
    return {
        "action_ok": True,
        "flight_recorder": {
            "macho_calls": calls or [],
            "scatters_observed": scatters or [],
            "log_exceptions": [],
            "quasar_errors": [],
            "macho_exceptions": [],
        },
    }


class CombatAftermathScenarioTests(unittest.TestCase):
    def test_scenario_validates_and_has_expected_assertions(self):
        scenario, by_id = _load_steps_by_id()
        validated = playtester.validate_scenario(scenario)
        self.assertEqual(validated["id"], "combat_aftermath")

        # Key milestones carry concrete RPC / state / UI assertions
        self.assertIn("ship.Board", by_id["board_merlin"]["checks"]["rpc_sent"])
        self.assertTrue(by_id["board_merlin"]["checks"]["ui_delta_without_relog"])

        loot_chk = by_id["click_loot_all"]["checks"]
        self.assertEqual(loot_chk["rpc_sent"][0]["name"], "invbroker.MultiAdd")
        self.assertTrue(loot_chk["rpc_sent"][0]["require_ok"])
        self.assertIn("OnItemChange", loot_chk["notification_received"])
        self.assertTrue(loot_chk["ui_delta_without_relog"])

        setup_cmds = [s["action"].get("command", "") for s in scenario.get("steps", []) if s.get("setup")]
        self.assertGreaterEqual(setup_cmds.count("/fit 90000001 31089"), 3)

        salv_step = by_id["wait_salvage_cycle"]
        self.assertIn("wait_until", salv_step)
        self.assertGreaterEqual(salv_step["wait_until"]["timeout_ms"], 60000)
        self.assertTrue(salv_step["checks"]["ui_delta_without_relog"])

        ins_chk = by_id["confirm_platinum_insurance"]["checks"]
        self.assertEqual(ins_chk["rpc_sent"][0]["name"], "insuranceSvc.InsureShip")
        self.assertIn({"field": "wallet_isk", "decreased": True}, ins_chk["state_checks"])

        sd_chk = by_id["confirm_self_destruct"]["checks"]
        self.assertIn("ship.SelfDestruct", sd_chk["rpc_sent"])
        self.assertTrue(sd_chk["expect_modal_absent"])

        self.assertIn("wait_until", by_id["tc_self_destruct_detonate"])
        det_chk = by_id["tc_self_destruct_detonate"]["checks"]
        self.assertIn({"field": "session.shipid", "changed": True}, det_chk["state_checks"])
        self.assertIn({"field": "wallet_isk", "delta": 0.0, "tolerance": 0.01}, det_chk["state_checks"])

        self.assertIn(
            "charMgr.GetRecentShipKillsAndLosses",
            by_id["open_cs_combat_log"]["checks"]["rpc_sent"],
        )
        self.assertEqual(
            by_id["open_kill_report"]["checks"]["window_populated"]["window"],
            "KillReportWnd",
        )
        self.assertEqual(
            by_id["open_kill_report"]["checks"]["window_populated"]["contains_text"],
            "Merlin",
        )

    def test_click_loot_all_positive_and_negative_mutations(self):
        _, by_id = _load_steps_by_id()
        chk = by_id["click_loot_all"]["checks"]

        pre = {
            "inventory_state": {"cargo_item_count": 1, "cargo_total_quantity": 400},
        }
        post_ok = {
            "inventory_state": {"cargo_item_count": 3, "cargo_total_quantity": 405},
        }
        fr_ok = _exec_ok(
            calls=[{"service": "invbroker", "method": "MultiAdd", "status": "OK"}],
            scatters=["OnWreckLootAll", "OnItemChange"],
        )
        ok, fk, msg = playtester.evaluate_scenario_checks(chk, pre, post_ok, fr_ok)
        self.assertTrue(ok, (fk, msg))

        # Mutation 1a: MultiAdd absent fails rpc_sent
        fr_missing_rpc = _exec_ok(
            calls=[],
            scatters=["OnItemChange"],
        )
        ok_r, fk_r, _ = playtester.evaluate_scenario_checks(chk, pre, post_ok, fr_missing_rpc)
        self.assertFalse(ok_r)
        self.assertEqual(fk_r, "rpc_sent")

        # Mutation 1b: MultiAdd ERR fails no_error_reply
        fr_err_rpc = _exec_ok(
            calls=[{"service": "invbroker", "method": "MultiAdd", "status": "ERR"}],
            scatters=["OnItemChange"],
        )
        ok_re, fk_re, _ = playtester.evaluate_scenario_checks(chk, pre, post_ok, fr_err_rpc)
        self.assertFalse(ok_re)
        self.assertEqual(fk_re, "no_error_reply")

        # Mutation 2: Cargo item count / total quantity did not increase
        ok_c, fk_c, _ = playtester.evaluate_scenario_checks(chk, pre, pre, fr_ok)
        self.assertFalse(ok_c)
        self.assertEqual(fk_c, "state_checks.cargo_item_count")

    def test_wait_salvage_cycle_positive_and_negative_mutations(self):
        _, by_id = _load_steps_by_id()
        chk = by_id["wait_salvage_cycle"]["checks"]

        pre = {
            "locked_targets": [{"target_id": 990001}],
            "inventory_state": {"cargo_item_count": 3, "cargo_total_quantity": 405},
        }
        post_ok = {
            "locked_targets": [],
            "inventory_state": {"cargo_item_count": 4, "cargo_total_quantity": 407},
        }
        ok, fk, msg = playtester.evaluate_scenario_checks(chk, pre, post_ok, _exec_ok())
        self.assertTrue(ok, (fk, msg))

        # Mutation 1: Wreck still locked (salvager failed or didn't clear wreck)
        post_still_locked = {
            "locked_targets": [{"target_id": 990001}],
            "inventory_state": {"cargo_item_count": 4, "cargo_total_quantity": 407},
        }
        ok_l, fk_l, _ = playtester.evaluate_scenario_checks(chk, pre, post_still_locked, _exec_ok())
        self.assertFalse(ok_l)
        self.assertEqual(fk_l, "state_checks.locked_targets")

        # Mutation 2: No salvage material entered ship cargo
        post_no_qty = {
            "locked_targets": [],
            "inventory_state": {"cargo_item_count": 3, "cargo_total_quantity": 405},
        }
        ok_q, fk_q, _ = playtester.evaluate_scenario_checks(chk, pre, post_no_qty, _exec_ok())
        self.assertFalse(ok_q)
        self.assertEqual(fk_q, "state_checks.inventory_state.cargo_total_quantity")

    def test_insurance_debit_self_destruct_zero_payout_and_kill_report(self):
        _, by_id = _load_steps_by_id()

        # Platinum insurance debit
        ins_chk = by_id["confirm_platinum_insurance"]["checks"]
        pre_ins = {
            "wallet_state": {"isk_balance": 500000000.0},
            "open_windows": [{"window_id": "InsuranceTermsWindow"}],
        }
        post_ins = {
            "wallet_state": {"isk_balance": 499700000.0},
            "open_windows": [],
        }
        fr_ins = _exec_ok(calls=[{"service": "insuranceSvc", "method": "InsureShip", "status": "OK"}])
        ok_i, fk_i, msg_i = playtester.evaluate_scenario_checks(ins_chk, pre_ins, post_ins, fr_ins)
        self.assertTrue(ok_i, (fk_i, msg_i))

        # Mutation: insurance wallet not debited
        ok_i_bad, fk_i_bad, _ = playtester.evaluate_scenario_checks(ins_chk, pre_ins, pre_ins | {"open_windows": []}, fr_ins)
        self.assertFalse(ok_i_bad)
        self.assertEqual(fk_i_bad, "state_checks.wallet_isk")

        # Uncontested self-destruct: shipid changed to capsule & wallet delta == 0.0 (no SCC payout)
        det_chk = by_id["tc_self_destruct_detonate"]["checks"]
        pre_det = {"session": {"shipid": 200010}, "wallet_state": {"isk_balance": 499700000.0}}
        post_det = {"session": {"shipid": 200011}, "wallet_state": {"isk_balance": 499700000.0}}
        ok_d, fk_d, msg_d = playtester.evaluate_scenario_checks(det_chk, pre_det, post_det, _exec_ok())
        self.assertTrue(ok_d, (fk_d, msg_d))

        # Mutation: bogus insurance payout on uncontested self-destruct must fail wallet_isk delta==0
        post_bad_payout = {"session": {"shipid": 200011}, "wallet_state": {"isk_balance": 500700000.0}}
        ok_dp, fk_dp, _ = playtester.evaluate_scenario_checks(det_chk, pre_det, post_bad_payout, _exec_ok())
        self.assertFalse(ok_dp)
        self.assertEqual(fk_dp, "state_checks.wallet_isk")

        # KillReportWnd populated with Merlin loss entry
        kr_chk = by_id["open_kill_report"]["checks"]
        post_kr = {
            "open_windows": [
                {
                    "window_id": "KillReportWnd",
                    "has_loading": False,
                    "has_error": False,
                    "populated_count": 3,
                    "sample_texts": ["Merlin", "Self-Destruct", "ISK"],
                }
            ]
        }
        ok_kr, fk_kr, msg_kr = playtester.evaluate_scenario_checks(kr_chk, {}, post_kr, _exec_ok())
        self.assertTrue(ok_kr, (fk_kr, msg_kr))

        post_kr_empty = {
            "open_windows": [
                {
                    "window_id": "KillReportWnd",
                    "has_loading": True,
                    "has_error": False,
                    "populated_count": 0,
                    "sample_texts": [],
                }
            ]
        }
        ok_kr_bad, fk_kr_bad, _ = playtester.evaluate_scenario_checks(kr_chk, {}, post_kr_empty, _exec_ok())
        self.assertFalse(ok_kr_bad)
        self.assertEqual(fk_kr_bad, "window_populated")


if __name__ == "__main__":
    unittest.main()
