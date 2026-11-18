#!/usr/bin/env python3
"""Unit tests for Issue #1201 Part A: journey isolation, log attribution, dynamic staging, and CLI flags."""

import builtins
import importlib.machinery
import importlib.util
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import types
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
RUN_JOURNEYS_PATH = os.path.join(HERE, "run_journeys")
DRIVER_PATH = os.path.join(HERE, "evomu_inclient_driver.py")
JOURNEYS_JSON_PATH = os.path.join(
    REPO_ROOT,
    ".agents",
    "skills",
    "evomu-adversarial-review",
    "references",
    "parity",
    "client_journeys.json",
)

if HERE not in sys.path:
    sys.path.insert(0, HERE)


def _load_run_journeys():
    loader = importlib.machinery.SourceFileLoader("evomu_run_journeys", RUN_JOURNEYS_PATH)
    spec = importlib.util.spec_from_loader("evomu_run_journeys", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


class _FakeSession:
    def __init__(self):
        self.userid = 1
        self.charid = 90000001
        self.shipid = 100001
        self.stationid = 60003760
        self.structureid = None
        self.solarsystemid = None
        self.solarsystemid2 = 30000142
        self.corpid = 1000167
        self.allianceid = None
        self.fleetid = None
        self.warfactionid = None
        self.mutating = 0
        self.changing = None
        self.nextSessionChange = None


class _FakeItem:
    def __init__(self, item_id, type_id, quantity=1, flag_id=4, singleton=1):
        self.itemID = item_id
        self.typeID = type_id
        self.quantity = quantity
        self.flagID = flag_id
        self.singleton = singleton


class _FakeCharRow:
    def __init__(self, char_id, ship_id=100001):
        self.characterID = char_id
        self.shipID = ship_id


class _FakeServiceProxy:
    def __init__(self, name, state, handlers):
        self._name = name
        self._state = state
        self._handlers = handlers

    def __getattr__(self, method_name):
        key = "%s.%s" % (self._name, method_name)
        if key in self._handlers:
            return self._handlers[key]

        def _default_method(*args, **kwargs):
            return self._state.handle_rpc(self._name, method_name, *args, **kwargs)

        return _default_method


class _FakeBoundObject:
    def __init__(self, svc_name, obj_id, state):
        self._svc_name = svc_name
        self._obj_id = obj_id
        self._state = state

    def __getattr__(self, method_name):
        def _method(*args, **kwargs):
            return self._state.handle_bound(
                self._svc_name, self._obj_id, method_name, *args, **kwargs
            )

        return _method


class _FakeWorldState:
    def __init__(self, sess, fail_methods=None):
        self.sess = sess
        self.fail_methods = dict(fail_methods or {})
        self.next_id = 200000
        self.hangar = [
            _FakeItem(100001, 587, 1, 4, 1),
        ]
        self.destroyed_ships = []
        self.unanchored_structs = []
        self.resigned_corps = []
        self.fleet_leaves = 0
        self.board_history = []
        self.calls = []
        self.sim_now = 1000.0
        self.session_change_until = 0.0
        self.in_dock_range_station = sess.stationid
        self.structures = {}
        self.unassembled_ships = set()
        self.space_ships = set()
        self.slash_cmds = []

    def _alloc(self):
        self.next_id += 1
        return self.next_id

    def handle_bound(self, svc_name, obj_id, method_name, *args, **kwargs):
        self.calls.append((svc_name + ".bound", method_name, obj_id))
        if svc_name == "dogmaIM" and method_name == "Activate":
            return {"probeIDs": [70001, 70002]}
        if svc_name == "dogmaIM" and method_name == "GetShipAttributes":
            return {"maxVelocity": 350.0}
        if svc_name == "invbroker" and method_name == "Add":
            return True
        if svc_name == "beyonce" and method_name in ("CmdWarpToStuff",):
            return True
        if svc_name == "reprocessingSvc" and method_name == "GetQuotes":
            return {"quotes": []}
        if svc_name == "reprocessingSvc" and method_name == "Reprocess":
            return {"yield": 100}
        if svc_name == "agentMgr" and method_name == "DoAction":
            return {"ok": True}
        if svc_name == "fleetObjectHandler" and method_name == "GetMotd":
            return "Fleet Operations"
        return {}

    def handle_rpc(self, svc_name, method_name, *args, **kwargs):
        full = "%s.%s" % (svc_name, method_name)
        self.calls.append((svc_name, method_name, args))
        if full in self.fail_methods:
            raise self.fail_methods[full]

        if method_name == "ClearSessionTimer":
            self.session_change_until = self.sim_now
            self.sess.nextSessionChange = None
            return True

        if method_name == "MachoBindObject":
            obj_id = args[0]
            call = args[1] if len(args) > 1 else None
            if svc_name == "invbroker" and isinstance(call, tuple) and call[0] == "List":
                return (None, list(self.hangar))
            if svc_name == "dogmaIM" and isinstance(call, tuple) and call[0] == "GetShipAttributes":
                return (None, {"maxVelocity": 350.0})
            return (_FakeBoundObject(svc_name, obj_id, self), [])

        if svc_name == "charUnboundMgr":
            if method_name == "GetCharactersToSelect":
                return [_FakeCharRow(90000001, 100001), _FakeCharRow(90000002, 100002)]
            if method_name == "ValidateNameEx":
                return True
            if method_name == "CreateCharacterWithDoll":
                return self._alloc()
            if method_name == "SelectCharacterID":
                self.sess.charid = int(args[0])
                return {"characterID": int(args[0])}

        if svc_name == "machoNet" and method_name == "GetTime":
            return 133000000000000000

        if svc_name == "slash" and method_name == "SlashCmd":
            cmd = str(args[0])
            self.slash_cmds.append(cmd)
            parts = cmd.split()
            if parts[0] == "/createitem":
                tid = int(parts[1])
                qty = int(parts[2]) if len(parts) > 2 else 1
                nid = self._alloc()
                self.hangar.append(_FakeItem(nid, tid, qty, 4, 0))
                return {"itemID": nid, "typeID": tid}
            if parts[0] == "/spawnnpc":
                return {"npcID": self._alloc()}
            if parts[0] == "/spawn":
                sid = self._alloc()
                self.space_ships.add(sid)
                return {"spawnedID": sid}
            if parts[0] == "/fit":
                return {"itemID": self._alloc()}
            if parts[0] == "/load":
                return {"itemID": self._alloc()}
            if parts[0] == "/tr":
                dest = int(parts[1])
                self.sess.stationid = None
                self.sess.structureid = None
                if 60000000 <= dest < 61000000:
                    self.sess.solarsystemid = 30000142
                    self.sess.solarsystemid2 = 30000142
                    self.in_dock_range_station = dest
                else:
                    self.sess.solarsystemid = dest
                    self.sess.solarsystemid2 = dest
                    self.in_dock_range_station = None
                return {"solarSystemID": self.sess.solarsystemid}
            if parts[0] == "/structure" and len(parts) >= 4 and parts[1] == "state":
                st_id = int(parts[2])
                state_id = int(parts[3])
                state_map = {
                    1: "Unanchored",
                    101: "Anchoring",
                    110: "ShieldVulnerable",
                    111: "ShieldReinforced",
                    115: "Unanchoring",
                }
                if st_id in self.structures:
                    self.structures[st_id]["state"] = state_map.get(state_id, "ShieldVulnerable")
                return {"ok": True}
            if parts[0] == "/upwelltimer":
                targets = [int(parts[1])] if len(parts) > 1 else list(self.structures.keys())
                for st_id in targets:
                    st = self.structures.get(st_id)
                    if not st:
                        continue
                    if st["state"] == "Unanchoring":
                        self.structures.pop(st_id, None)
                    elif st["state"] == "Anchoring":
                        st["state"] = "ShieldVulnerable"
                return {"ok": True}
            if parts[0] == "/sessiontimer":
                self.session_change_until = self.sim_now
                self.sess.nextSessionChange = None
                return {"ok": True}
            return {"ok": True}

        if svc_name == "ship":
            if method_name == "Undock":
                prev_st = self.sess.stationid
                self.sess.stationid = None
                self.sess.solarsystemid = self.sess.solarsystemid2
                self.in_dock_range_station = prev_st or 60003760
                return True
            if method_name == "AssembleShip":
                raw = args[0] if args else []
                ids = raw if isinstance(raw, (list, tuple, set)) else [raw]
                for x in ids:
                    self.unassembled_ships.discard(int(x))
                return True
            if method_name == "Board":
                sid = int(args[0])
                if self.sess.stationid is None and sid not in self.space_ships:
                    raise RuntimeError("MustBeDocked")
                if self.sim_now < self.session_change_until:
                    raise RuntimeError("SessionChangeInProgress")
                if sid in self.unassembled_ships:
                    raise RuntimeError("ShipNotAssembled")
                self.sess.shipid = sid
                self.board_history.append(sid)
                return True
            if method_name == "SelfDestruct":
                self.destroyed_ships.append(self.sess.shipid)
                pod_id = self._alloc()
                self.sess.shipid = pod_id
                return True
            return True

        if svc_name == "stationSvc" and method_name == "Dock":
            sid = int(args[0])
            if self.sess.stationid != sid and self.in_dock_range_station != sid:
                raise RuntimeError("DockOutOfRange")
            self.sess.stationid = sid
            self.sess.structureid = None
            self.sess.solarsystemid = None
            self.in_dock_range_station = sid
            return True

        if svc_name == "beyonce" and method_name == "CmdStargateJump":
            gate_to_sys = {
                50001249: 30000144,
                50002184: 30000139,
                50000547: 30000141,
                50001004: 30000139,
                50000549: 30000144,
                50002185: 30000142,
            }
            dest = gate_to_sys.get(int(args[0]), 30000142)
            self.sess.solarsystemid = dest
            self.sess.solarsystemid2 = dest
            self.in_dock_range_station = None
            return True

        if svc_name == "corpRegistry":
            if method_name == "AddCorporation":
                cid = 98000000 + (self._alloc() % 1000)
                self.sess.corpid = cid
                self.session_change_until = self.sim_now + 10.0
                return {"corporationID": cid}
            if method_name == "CreateAlliance":
                aid = 99000000 + (self._alloc() % 1000)
                self.sess.allianceid = aid
                self.session_change_until = self.sim_now + 10.0
                return {"allianceID": aid}
            if method_name == "ResignFromCorp":
                if self.sim_now < self.session_change_until:
                    raise RuntimeError("SessionChangeInProgress")
                if self.sess.stationid is None:
                    raise RuntimeError("MustBeDocked")
                active = [
                    sid
                    for sid, st in self.structures.items()
                    if st["ownerCorpID"] == self.sess.corpid and st["state"] != "Destroyed"
                ]
                if active:
                    raise RuntimeError("ActiveStructuresPreventClosure")
                self.resigned_corps.append(self.sess.corpid)
                self.sess.corpid = 1000167
                self.sess.allianceid = None
                return True
            if method_name == "GetCorporation":
                return {"corporationName": "State War Academy"}

        if svc_name == "fleetMgr":
            if method_name == "CreateFleet":
                fid = self._alloc()
                self.sess.fleetid = fid
                return fid
            if method_name == "LeaveFleet":
                self.fleet_leaves += 1
                self.sess.fleetid = None
                return True

        if svc_name == "structureDirectory":
            if method_name == "GetMyCorporationStructures":
                return {
                    sid: dict(st)
                    for sid, st in self.structures.items()
                    if st["ownerCorpID"] == self.sess.corpid and st["state"] != "Destroyed"
                }

        if svc_name == "structureDeployment":
            if method_name == "Anchor":
                st_id = int(args[0])
                self.structures[st_id] = {
                    "structureID": st_id,
                    "corporationID": self.sess.corpid,
                    "ownerCorpID": self.sess.corpid,
                    "state": "Anchoring",
                }
                return {"structureID": st_id}
            if method_name == "Unanchor":
                st_id = int(args[0])
                st = self.structures.get(st_id)
                if st is not None:
                    if st["state"] not in ("ShieldVulnerable", "Anchored"):
                        raise RuntimeError("WrongStructureState: %s" % st["state"])
                    st["state"] = "Unanchoring"
                self.unanchored_structs.append(st_id)
                return True

        if svc_name == "structureDocking":
            if method_name == "Dock":
                self.sess.structureid = int(args[0])
                self.sess.solarsystemid = None
                self.in_dock_range_station = None
                return True
            if method_name == "Undock":
                self.sess.structureid = None
                self.sess.solarsystemid = self.sess.solarsystemid2
                return True

        if svc_name == "abyssalMgr":
            if method_name == "AbyssalEntranceDeployment":
                return {"traceBallID": 1895000100}
            if method_name == "AbyssalEntranceGateActivation":
                self.sess.solarsystemid = 32000001
                self.sess.solarsystemid2 = 32000001
                self.in_dock_range_station = None
                return {"pocketID": 1, "solarSystemID": 32000001, "npcBallIDs": []}
            if method_name == "AbyssalGateActivation":
                return {"npcBallIDs": []}
            if method_name == "AbyssalEndGateActivation":
                self.sess.solarsystemid = 30000142
                self.sess.solarsystemid2 = 30000142
                self.in_dock_range_station = None
                return True
            return True

        if svc_name == "facWarMgr" and method_name == "JoinFactionAsCharacter":
            self.sess.warfactionid = int(args[0])
            return True

        if svc_name == "randomJumpMgr" and method_name == "ActivateRandomJumpFilament":
            self.sess.solarsystemid = 30002000
            self.sess.solarsystemid2 = 30002000
            self.in_dock_range_station = None
            return {"solarSystemID": 30002000}

        if svc_name == "charMgr" and method_name == "GetRecentShipKillsAndLosses":
            return [{"killID": 1}]

        if svc_name == "storeManager":
            if method_name == "get_offers":
                return [{"offerID": 10, "price": 100, "products": [{"typeID": 170}]}]
            if method_name == "can_purchase_offer":
                return True
            if method_name == "buy_offer":
                return {"deliveredItemID": 55501, "plexRemaining": 50000}

        if svc_name == "FastCheckoutService":
            if method_name == "GetOffersForUser":
                return {"offers": [{"offerId": 1, "price": 0.0}]}
            if method_name == "BuyOffer":
                return {"status": "COMPLETED", "newPlexBalance": 50100}

        if svc_name == "shipCosmeticsMgr" and method_name == "GetLicencedSkins":
            return [170]

        if svc_name == "marketProxy":
            if method_name == "GetMarketGroups":
                return [1, 2]
            if method_name == "PlaceMultiSellOrder":
                return [{"orderID": 8801}]
            if method_name == "GetCharOrders":
                return [{"orderID": 8801, "typeID": 19658, "bid": False}]
            if method_name == "CharGetTransactions":
                return [{"typeID": 19658, "quantity": 5}]
            if method_name == "GetOldPriceHistory":
                return [{"price": 50.0}]

        if svc_name == "contractProxy" and method_name == "CreateContract":
            return {"contractID": self._alloc()}

        if svc_name == "accessGroupBookmarkMgr" and method_name == "BookmarkStaticLocation":
            return [90001]

        if svc_name == "agentMgr" and method_name == "GetAgents":
            return [{"agentID": 3020034, "level": 1, "solarSystemID": 30000142, "stationID": 60003760}]

        if svc_name == "scanSvc" and method_name == "ConeScan":
            return [{"id": 1}]
        if svc_name == "scanMgr" and method_name == "RequestScans":
            return [{"id": "DAT-30000142"}]
        if svc_name == "fwWarzoneSolarsystem" and method_name == "GetAllWarzones":
            return [10]

        return {"ok": True}


class _FakeViewState:
    def __init__(self, sess):
        self.sess = sess
        self.isOpeningView = None

    def GetCurrentViewInfo(self):
        return types.SimpleNamespace(
            name="hangar" if self.sess.stationid else "inflight"
        )

    def IsViewActive(self, name):
        if name in ("hangar", "station"):
            return bool(self.sess.stationid)
        return not bool(self.sess.stationid)

    def ChangePrimaryView(self, name):
        return None


class _FakeBallpark:
    def __init__(self, sess):
        self._sess = sess
        self.balls = {999999: True}
        self.validState = True

    @property
    def solarsystemID(self):
        return self._sess.solarsystemid2

    def GetInvItem(self, bid):
        return types.SimpleNamespace(categoryID=25, groupID=9)

    def GetBall(self, bid):
        return types.SimpleNamespace(mode=0)


class _FakeSM:
    def __init__(self, state):
        self._state = state
        bp = _FakeBallpark(state.sess)
        self.services = {
            "loading": types.SimpleNamespace(IsLoading=lambda: False),
            "space": types.SimpleNamespace(lazyLoadQueueCount=0),
            "viewState": _FakeViewState(state.sess),
            "sceneManager": types.SimpleNamespace(
                GetActiveScene=lambda: types.SimpleNamespace(name="scene")
            ),
            "godma": types.SimpleNamespace(state=4),
            "michelle": types.SimpleNamespace(
                _Michelle__bp=bp,
                bpReady=True,
                GetBallpark=lambda: bp,
                GetRemotePark=lambda: None,
            ),
            "fleet": types.SimpleNamespace(
                fleet=None,
                GetMyShipTypeID=lambda: 587,
                InitFleet=lambda: None,
            ),
            "invCache": types.SimpleNamespace(
                GetInventoryFromId=lambda sid: types.SimpleNamespace(Add=lambda *a, **kw: None)
            ),
            "publicGatewaySvc": types.SimpleNamespace(
                is_available=lambda: True,
                publicGateway=types.SimpleNamespace(
                    set_authenticated_user_id=lambda uid: None,
                    set_active_character_id=lambda cid: None,
                    get_request_broker_details=lambda: {"connected": True},
                ),
            ),
            "sessionMgr": types.SimpleNamespace(
                PerformSessionChange=lambda _reason, fn, *args: fn(*args)
            ),
        }

    def IsServiceRunning(self, name):
        return True

    def GetService(self, name):
        return self.services.get(name, types.SimpleNamespace())

    def RemoteSvc(self, name):
        return _FakeServiceProxy(name, self._state, {})


class _FakeIPC:
    def __init__(self, state=None):
        self.messages = []
        self.state = state

    def request(self, payload):
        self.messages.append(payload)
        cmd = payload.get("cmd")
        if cmd == "ensure_client2_ready":
            return {"ok": True, "char2_id": 90000002}
        if cmd == "advance_clock":
            if self.state is not None:
                self.state.sim_now += float(payload.get("secs", 0))
                if self.state.sim_now >= self.state.session_change_until:
                    self.state.sess.nextSessionChange = None
            return {"ok": True, "time": True}
        if cmd == "c2_check_ball_and_dock":
            return {"ok": True, "saw_ship": True, "updates": 2}
        return {"ok": True}

    def notify(self, payload):
        self.messages.append(payload)
        return {"ok": True}


class RunJourneysIsolationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="evomu_1201_test_")
        self.addCleanup(lambda: shutil.rmtree(self.tmp, ignore_errors=True))

    def test_zero_v24_01_and_all_journey_ids_present_in_driver(self):
        with open(DRIVER_PATH, "r", encoding="utf-8") as df:
            driver_src = df.read()
        self.assertEqual(driver_src.count("V24.01"), 0)

        with open(JOURNEYS_JSON_PATH, "r", encoding="utf-8") as jf:
            journeys = json.load(jf)["journeys"]
        for j in journeys:
            self.assertIn('"%s"' % j["id"], driver_src)

    def test_driver_continues_when_journey_2_raises_and_isolates_state(self):
        import evomu_inclient_driver as drv
        import evomu_inclient_helpers as hlp

        sess = _FakeSession()
        state = _FakeWorldState(
            sess,
            fail_methods={
                "charUnboundMgr.CreateCharacterWithDoll": RuntimeError("boom in journey 2")
            },
        )
        sm = _FakeSM(state)
        ipc = _FakeIPC(state)

        # Keep test fast and deterministic by avoiding real HTTP/sleep delays.
        mock_http_resp = mock.MagicMock()
        mock_http_resp.getcode.return_value = 200
        mock_http_resp.read.return_value = b'{"players": 1}'

        with (
            mock.patch.object(drv.__builtin__, "sm", sm, create=True),
            mock.patch.object(drv.__builtin__, "session", sess, create=True),
            mock.patch.object(hlp.__builtin__, "sm", sm, create=True),
            mock.patch.object(hlp.__builtin__, "session", sess, create=True),
            mock.patch.object(hlp, "_sleep_ms", return_value=None),
            mock.patch.object(hlp, "_sleep_sim_ms", return_value=None),
            mock.patch.object(hlp, "_wait_warp_fx_settled", return_value=None),
            mock.patch.object(drv, "_warp_full", return_value={"warp": True}),
            mock.patch.object(drv, "_warp_animated", return_value={"warp": True}),
            mock.patch.object(drv._urllib, "urlopen", return_value=mock_http_resp),
        ):
            out_dir = os.path.join(self.tmp, "driver_out")
            outcomes = drv._execute_all_journeys(
                {
                    "out_dir": out_dir,
                    "user": "demo",
                    "password": "demo-secret",
                    "char_id": 90000001,
                    "esi_port": 26004,
                    "silent": True,
                },
                ipc,
            )

        # Journey 2 failed with traceback; journeys 3..N still executed.
        self.assertIn("create_character", outcomes)
        self.assertFalse(outcomes["create_character"]["passed"])
        self.assertIn("RuntimeError: boom in journey 2", outcomes["create_character"]["traceback"])

        expected_order = [jid for jid, _ in drv._JOURNEY_HANDLERS]
        for jid in expected_order[2:]:
            self.assertIn(jid, outcomes, msg="Journey %s was skipped after failure" % jid)
            self.assertIsInstance(outcomes[jid]["rpcs_sent"], list)
            self.assertNotIn("Real V24.01", outcomes[jid]["evidence"])
            self.assertNotIn("eliminated rat", outcomes[jid]["evidence"])

        # Persisted JSON matches and preserves all outcomes
        outcomes_file = os.path.join(out_dir, "real_client_outcomes.json")
        self.assertTrue(os.path.exists(outcomes_file))
        with open(outcomes_file, "r", encoding="utf-8") as of:
            persisted = json.load(of)
        self.assertEqual(set(persisted.keys()), set(expected_order))

        # Baseline isolation guarantees:
        # 1. ship_loss did not destroy primary ship 100001 and returned to 100001
        self.assertNotIn(100001, state.destroyed_ships)
        self.assertEqual(sess.shipid, 100001)
        # 2. corporation / structures resigned back to baseline NPC corp 1000167
        self.assertEqual(sess.corpid, 1000167)
        self.assertGreaterEqual(len(state.resigned_corps), 1)
        # 3. structures unanchored deployed citadel
        self.assertGreaterEqual(len(state.unanchored_structs), 1)
        # 4. wormholes_filaments & fleet cleaned up fleet state
        self.assertIsNone(sess.fleetid)
        self.assertGreaterEqual(state.fleet_leaves, 2)
        # 5. journey start/end markers sent over IPC for every journey
        started_ids = [m["id"] for m in ipc.messages if m.get("cmd") == "journey_start"]
        ended_ids = [m["id"] for m in ipc.messages if m.get("cmd") == "journey_end"]
        self.assertEqual(started_ids, expected_order)
        self.assertEqual(ended_ids, expected_order)

    def test_single_client_only_filter_never_requests_client2(self):
        import evomu_inclient_driver as drv
        import evomu_inclient_helpers as hlp

        sess = _FakeSession()
        state = _FakeWorldState(sess)
        sm = _FakeSM(state)
        ipc = _FakeIPC(state)

        with (
            mock.patch.object(drv.__builtin__, "sm", sm, create=True),
            mock.patch.object(drv.__builtin__, "session", sess, create=True),
            mock.patch.object(hlp.__builtin__, "sm", sm, create=True),
            mock.patch.object(hlp.__builtin__, "session", sess, create=True),
            mock.patch.object(hlp, "_sleep_ms", return_value=None),
            mock.patch.object(hlp, "_sleep_sim_ms", return_value=None),
        ):
            outcomes = drv._execute_all_journeys(
                {
                    "out_dir": os.path.join(self.tmp, "only_out"),
                    "only_journeys": ["launch_login", "station_ui"],
                    "silent": True,
                },
                ipc,
            )

        self.assertEqual(list(outcomes.keys()), ["launch_login", "station_ui"])
        self.assertTrue(all(v["passed"] for v in outcomes.values()))
        cmds = [m.get("cmd") for m in ipc.messages]
        self.assertNotIn("ensure_client2_ready", cmds)

    def test_synthetic_log_attribution_and_exit_code_gate(self):
        rj = _load_run_journeys()
        journeys = [
            {"id": "launch_login", "title": "Launch"},
            {"id": "create_character", "title": "Create"},
            {"id": "station_ui", "title": "Station"},
            {"id": "fit_ship", "title": "Fit"},
            {"id": "undock_navigate", "title": "Undock"},
            {"id": "local_chat", "title": "Chat"},
        ]

        client_log = os.path.join(self.tmp, "real_client_1.log")
        server_log = os.path.join(self.tmp, "server.log")
        with open(client_log, "w", encoding="utf-8") as cf:
            for j in journeys:
                jid = j["id"]
                cf.write("[EVOMU_JOURNEY_START] id=%s\n" % jid)
                cf.write("[client-1] running %s\n" % jid)
                if jid == "undock_navigate":
                    cf.write("EXCEPTION inside synthetic journey 5 nav failure\n")
                cf.write("[EVOMU_JOURNEY_END] id=%s passed=True\n" % jid)

        with open(server_log, "w", encoding="utf-8") as sf:
            for j in journeys:
                jid = j["id"]
                sf.write("[harness] [EVOMU_JOURNEY_START] id=%s\n" % jid)
                sf.write("[server] INFO dispatch %s\n" % jid)
                sf.write("[harness] [EVOMU_JOURNEY_END] id=%s\n" % jid)

        real_outcomes = {
            j["id"]: {
                "passed": True,
                "real_client_verified": True,
                "bridge_verified": True,
                "rpcs_sent": [{"service": "machoNet", "method": "GetTime", "status": "ok"}],
                "metrics": {"ok": True},
                "evidence": "rpcs=1",
            }
            for j in journeys
        }

        result, exit_code = rj.finalize_journey_run(
            out_dir=self.tmp,
            journey_list=journeys,
            server_log_path=server_log,
            client_logs=[client_log],
            client_app_path=self.tmp,
            build_info={"build": "1234567", "version": "24.09"},
            concurrent_clients_tested=1,
            should_launch_real_client=True,
            real_outcomes=real_outcomes,
            bridge_outcomes={},
        )

        self.assertEqual(exit_code, 2)
        self.assertEqual(len(result["log_errors"]), 1)
        self.assertIn("EXCEPTION inside synthetic journey 5", result["log_errors"][0])

        # Only journey 5 (`undock_navigate`) fails due to its attributed error line.
        for j in journeys:
            jid = j["id"]
            entry = result["journeys"][jid]
            if jid == "undock_navigate":
                self.assertFalse(entry["passed"])
                self.assertEqual(len(entry["log_errors"]), 1)
                self.assertIn("EXCEPTION inside synthetic journey 5", entry["log_errors"][0])
            else:
                self.assertTrue(entry["passed"], msg="%s should remain passed" % jid)
                self.assertEqual(entry["log_errors"], [])
            self.assertTrue(entry["client_log_excerpt"])
            self.assertTrue(entry["server_log_excerpt"])

    def test_dynamic_client_build_staging_and_result_json(self):
        rj = _load_run_journeys()
        fake_build = os.path.join(self.tmp, "build")
        os.makedirs(os.path.join(fake_build, "bin64"), exist_ok=True)
        with open(os.path.join(fake_build, "start.ini"), "w", encoding="utf-8") as sf:
            sf.write("[main]\nversion = 24.09\nbuild = 1234567\ncodename = V24.09\n")
        with open(os.path.join(fake_build, "bin64", "exefile"), "w", encoding="utf-8") as ef:
            ef.write("#!/bin/sh\nexit 0\n")

        stage_root = os.path.join(self.tmp, "cache")
        stage_dir, _ = rj.stage_real_client(fake_build, stage_root=stage_root)
        self.assertTrue(stage_dir.endswith("client_build_1234567"))
        self.assertTrue(os.path.exists(os.path.join(stage_dir, "common.ini")))
        self.assertTrue(os.path.exists(os.path.join(stage_dir, "tools", "startup", "evomu.py")))

        out_dir = os.path.join(self.tmp, "cli_out")
        res = subprocess.run(
            [
                sys.executable,
                RUN_JOURNEYS_PATH,
                "--out",
                out_dir,
                "--journeys",
                JOURNEYS_JSON_PATH,
                "--bridge-port",
                "65432",
                "--client-app",
                fake_build,
                "--skip-real-client",
                "--only",
                "launch_login,station_ui",
            ],
            capture_output=True,
            text=True,
            timeout=15,
        )
        self.assertEqual(res.returncode, 1)
        with open(os.path.join(out_dir, "result.json"), "r", encoding="utf-8") as rf:
            res_json = json.load(rf)
        self.assertEqual(res_json["client_build"], "1234567")
        self.assertEqual(res_json["client_version"], "24.09")
        self.assertEqual(list(res_json["journeys"].keys()), ["launch_login", "station_ui"])
        self.assertEqual(res_json["concurrent_clients_tested"], 0)

    def test_concurrent_clients_tested_and_silent_opt_in(self):
        rj = _load_run_journeys()
        log2_path = os.path.join(self.tmp, "c2.log")
        srv_log_path = os.path.join(self.tmp, "server.log")
        outcomes_dict = {}
        ipc = rj.IpcCoordinator("127.0.0.1", 1, log2_path, outcomes_dict, server_log_path=srv_log_path)
        try:
            self.assertEqual(ipc._handle_cmd({"cmd": "client1_authenticated"}), {"ok": True})
            self.assertEqual(ipc._handle_cmd({"cmd": "journey_start", "id": "station_ui"}), {"ok": True})
            self.assertEqual(ipc._handle_cmd({"cmd": "advance_clock", "secs": 12}), {"ok": False, "reason": "c2_not_connected"})
            self.assertEqual(ipc._handle_cmd({"cmd": "journey_end", "id": "station_ui", "passed": True}), {"ok": True})
            self.assertTrue(ipc.c1_authenticated)
            self.assertFalse(ipc.c2_authenticated)
            c1_count = (1 if ipc.c1_authenticated else 0) + (1 if ipc.c2_authenticated else 0)
            self.assertEqual(c1_count, 1)
        finally:
            ipc.close()

        # Verify /silent is opt-in rather than unconditional in launch_real_client
        fake_build = os.path.join(self.tmp, "build_launch")
        os.makedirs(os.path.join(fake_build, "bin64"), exist_ok=True)
        with open(os.path.join(fake_build, "start.ini"), "w", encoding="utf-8") as sf:
            sf.write("[main]\nversion = 24.02\nbuild = 3698585\n")
        with open(os.path.join(fake_build, "bin64", "exefile"), "w", encoding="utf-8") as ef:
            ef.write("#!/bin/sh\nexit 0\n")

        stage_root_launch = os.path.join(self.tmp, "cache_launch")
        rj.stage_real_client(fake_build, stage_root=stage_root_launch)

        captured_cmds = []

        def _fake_popen(cmd, **kwargs):
            captured_cmds.append(list(cmd))
            proc = mock.MagicMock()
            proc.pid = 4242
            return proc

        with (
            mock.patch.object(rj, "_pids_matching", return_value=set()),
            mock.patch.object(rj.subprocess, "Popen", side_effect=_fake_popen),
        ):
            rj.launch_real_client(
                fake_build,
                "127.0.0.1",
                26000,
                "demo:demo-secret",
                os.path.join(self.tmp, "rl1.log"),
                silent=False,
                stage_root=stage_root_launch,
            )
            rj.launch_real_client(
                fake_build,
                "127.0.0.1",
                26000,
                "demo:demo-secret",
                os.path.join(self.tmp, "rl2.log"),
                silent=True,
                stage_root=stage_root_launch,
            )

        self.assertNotIn("/silent", captured_cmds[0])
        self.assertIn("/silent", captured_cmds[1])
        rj._SPAWNED.clear()

    def test_server_log_append_preserves_interleaved_subprocess_markers(self):
        rj = _load_run_journeys()
        server_log = os.path.join(self.tmp, "server.log")
        with open(server_log, "w", encoding="utf-8") as stale_f:
            stale_f.write("STALE_RUN_LINE\n")

        # Mirror run_journeys' truncate-then-append pattern around Popen
        with open(server_log, "w", encoding="utf-8"):
            pass
        child_script = (
            "import sys, time\n"
            "for i in range(25):\n"
            "    sys.stdout.write('srv_tick_%02d\\n' % i)\n"
            "    sys.stdout.flush()\n"
            "    time.sleep(0.005)\n"
        )
        with open(server_log, "a", encoding="utf-8") as slog:
            proc = subprocess.Popen(
                [sys.executable, "-c", child_script],
                stdout=slog,
                stderr=subprocess.STDOUT,
            )
            try:
                for k in range(3):
                    rj._append_log_line(server_log, "[harness] [EVOMU_JOURNEY_START] id=j%d" % k)
                    rj._append_log_line(server_log, "[harness] [EVOMU_JOURNEY_END] id=j%d passed=True" % k)
                proc.wait(timeout=10)
            finally:
                if proc.poll() is None:
                    proc.kill()
                    proc.wait(timeout=5)

        self.assertEqual(proc.returncode, 0)
        with open(server_log, "r", encoding="utf-8") as rf:
            content = rf.read()
        self.assertNotIn("STALE_RUN_LINE", content)
        for i in range(25):
            self.assertIn("srv_tick_%02d" % i, content)
        for k in range(3):
            self.assertIn("[EVOMU_JOURNEY_START] id=j%d" % k, content)
            self.assertIn("[EVOMU_JOURNEY_END] id=j%d" % k, content)

    def test_ipc_coordinator_emits_markers_and_byte_offsets_for_client_logs(self):
        import evomu_inclient_driver as drv

        rj = _load_run_journeys()
        out_dir = os.path.join(self.tmp, "ipc_out")
        client_logs_dir = os.path.join(out_dir, "client_logs")
        os.makedirs(client_logs_dir, exist_ok=True)
        c1_ext = os.path.join(client_logs_dir, "20261118_1.log")
        c2_log = os.path.join(out_dir, "client_2.log")
        tb_log = os.path.join(out_dir, "client_tracebacks.log")
        srv_log = os.path.join(out_dir, "server.log")
        for p in (c1_ext, tb_log, srv_log):
            with open(p, "w", encoding="utf-8"):
                pass

        ipc = rj.IpcCoordinator(
            "127.0.0.1",
            1,
            c2_log,
            {},
            server_log_path=srv_log,
            out_dir=out_dir,
            client_logs_dir=client_logs_dir,
        )
        try:
            ipc._handle_cmd({"cmd": "journey_start", "id": "launch_login"})
            with open(c1_ext, "a", encoding="utf-8") as ef:
                ef.write("notice clean login packet\n")
            ipc._handle_cmd({"cmd": "journey_end", "id": "launch_login", "passed": True})

            ipc._handle_cmd({"cmd": "journey_start", "id": "undock_navigate"})
            with open(c1_ext, "a", encoding="utf-8") as ef:
                ef.write("Traceback (most recent call last): client_logs external navigation fault\n")
            ipc.log2.info("EXCEPTION c2 ball sync mismatch")
            ipc._handle_cmd({"cmd": "journey_end", "id": "undock_navigate", "passed": True})

            ipc._handle_cmd({"cmd": "journey_start", "id": "station_ui"})
            with open(c1_ext, "a", encoding="utf-8") as ef:
                ef.write("notice station panel loaded\n")
            ipc._handle_cmd({"cmd": "journey_end", "id": "station_ui", "passed": True})
            byte_spans = dict(ipc.journey_byte_spans)
        finally:
            ipc.close()

        for path in (srv_log, c2_log, tb_log):
            with open(path, "r", encoding="utf-8") as rf:
                txt = rf.read()
            self.assertIn("[EVOMU_JOURNEY_START] id=undock_navigate", txt, msg=path)
            self.assertIn("[EVOMU_JOURNEY_END] id=undock_navigate", txt, msg=path)

        journeys = [
            {"id": "launch_login", "title": "Launch"},
            {"id": "undock_navigate", "title": "Undock"},
            {"id": "station_ui", "title": "Station"},
        ]
        real_outcomes = {
            j["id"]: {
                "passed": True,
                "real_client_verified": True,
                "bridge_verified": True,
                "rpcs_sent": [{"service": "machoNet", "method": "GetTime", "status": "ok"}],
                "metrics": {"ok": True},
                "evidence": "rpcs=1",
            }
            for j in journeys
        }
        result, exit_code = rj.finalize_journey_run(
            out_dir=out_dir,
            journey_list=journeys,
            server_log_path=srv_log,
            client_logs=[c1_ext, c2_log, tb_log],
            client_app_path=self.tmp,
            build_info={"build": "1234567", "version": "24.09"},
            concurrent_clients_tested=1,
            should_launch_real_client=True,
            real_outcomes=real_outcomes,
            bridge_outcomes={},
            journey_byte_spans=byte_spans,
        )
        self.assertEqual(exit_code, 2)
        self.assertTrue(result["journeys"]["launch_login"]["passed"])
        self.assertEqual(result["journeys"]["launch_login"]["log_errors"], [])
        self.assertTrue(result["journeys"]["station_ui"]["passed"])
        self.assertEqual(result["journeys"]["station_ui"]["log_errors"], [])

        undock = result["journeys"]["undock_navigate"]
        self.assertFalse(undock["passed"])
        joined_errs = "\n".join(undock["log_errors"])
        self.assertIn("Traceback (most recent call last): client_logs external navigation fault", joined_errs)
        self.assertIn("EXCEPTION c2 ball sync mismatch", joined_errs)

        # Verify _emit_client_marker invokes logmodule.general.Log cleanly (2-arg and 1-arg fallback)
        recorded_logs = []

        def _two_arg_log(msg, lvl):
            recorded_logs.append((msg, lvl))

        fake_lm_2 = types.SimpleNamespace(general=types.SimpleNamespace(Log=_two_arg_log), INFO=1)
        with mock.patch.dict(sys.modules, {"logmodule": fake_lm_2}):
            drv._emit_client_marker({"out_dir": out_dir}, "EVOMU_JOURNEY_START", "launch_login")
        self.assertTrue(any("[EVOMU_JOURNEY_START] id=launch_login" in m for m, _ in recorded_logs))

        recorded_1 = []

        def _one_arg_only(msg):
            recorded_1.append(msg)

        fake_lm_1 = types.SimpleNamespace(general=types.SimpleNamespace(Log=_one_arg_only), INFO=1)
        with mock.patch.dict(sys.modules, {"logmodule": fake_lm_1}):
            drv._emit_client_marker({"out_dir": out_dir}, "EVOMU_JOURNEY_END", "launch_login", extra="passed=True")
        self.assertTrue(any("[EVOMU_JOURNEY_END] id=launch_login passed=True" in m for m in recorded_1))

    def test_structures_and_corp_cleanup_enforce_server_invariants_and_board_fallback(self):
        import evomu_inclient_helpers as hlp

        sess = _FakeSession()
        state = _FakeWorldState(sess)
        sm = _FakeSM(state)
        ipc = _FakeIPC(state)

        with (
            mock.patch.object(hlp.__builtin__, "sm", sm, create=True),
            mock.patch.object(hlp.__builtin__, "session", sess, create=True),
            mock.patch.object(hlp, "_sleep_ms", return_value=None),
            mock.patch.object(hlp, "_sleep_sim_ms", return_value=None),
        ):
            # 1. Structure anchored in Anchoring state + active 10s corp timer:
            #    ResignFromCorp must fail unless _resign_player_corp flips state -> Unanchor -> /upwelltimer -> advance_clock.
            sm.RemoteSvc("corpRegistry").AddCorporation()
            self.assertNotEqual(sess.corpid, 1000167)
            st_id = 1090000123
            sm.RemoteSvc("structureDeployment").Anchor(st_id)
            sm.RemoteSvc("structureDocking").Dock(st_id)
            with self.assertRaises(RuntimeError):
                sm.RemoteSvc("corpRegistry").ResignFromCorp()

            hlp._resign_player_corp(sm, baseline_corp_id=1000167, ipc=ipc, structure_ids=[st_id])
            self.assertEqual(sess.corpid, 1000167)
            self.assertEqual(state.structures, {})
            self.assertIn(st_id, state.unanchored_structs)
            self.assertEqual(sess.stationid, 60003760)
            self.assertTrue(any(c.startswith("/structure state %d 110" % st_id) for c in state.slash_cmds))

            # 2. _ensure_docked_jita falls back to /tr <station_id> (never system_id) when DockOutOfRange hits
            state.slash_cmds.clear()
            sm.RemoteSvc("slash").SlashCmd("/tr 30000142")
            with self.assertRaises(RuntimeError):
                sm.RemoteSvc("stationSvc").Dock(60003760)
            hlp._ensure_docked_jita(sm, ipc, station_id=60003760, system_id=30000142)
            self.assertEqual(sess.stationid, 60003760)
            self.assertIn("/tr 60003760", state.slash_cmds)
            self.assertEqual(state.slash_cmds[-1], "/tr 60003760")

            # 3. _ensure_home_ship handles ShipNotAssembled via AssembleShip + Board retry (no unimplemented RPCs)
            sess.shipid = 200999
            state.unassembled_ships.add(100001)
            hlp._ensure_home_ship(sm, 100001, ipc=ipc)
            self.assertEqual(sess.shipid, 100001)
            called_methods = ["%s.%s" % (s, m) for s, m, _ in state.calls]
            self.assertIn("ship.AssembleShip", called_methods)
            self.assertNotIn("dogmaIM.ShipAccess", called_methods)
            self.assertNotIn("station.TryActivateShip", called_methods)

    def test_helpers_python3_extract_id_ipc_and_recorded_proxy(self):
        import evomu_inclient_helpers as hlp

        # 1. Python 3 _extract_id on tuple/int/dict/None without NameError on `long`
        self.assertEqual(hlp._extract_id((42,)), 42)
        self.assertEqual(hlp._extract_id(42), 42)
        self.assertEqual(hlp._extract_id({"structureID": 777}), 777)
        self.assertIsNone(hlp._extract_id(None))

        # 2. Real loopback TCP socket UTF-8 bytes encode/decode in IpcClient.request
        srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        srv.bind(("127.0.0.1", 0))
        srv.listen(1)
        port = srv.getsockname()[1]
        received = []

        def _serve_one():
            conn, _ = srv.accept()
            try:
                raw = b""
                while not raw.endswith(b"\n"):
                    chunk = conn.recv(4096)
                    if not chunk:
                        break
                    raw += chunk
                req = json.loads(raw.decode("utf-8").strip())
                received.append(req)
                resp = json.dumps({"ok": True, "echo": req.get("cmd")}) + "\n"
                conn.sendall(resp.encode("utf-8"))
            finally:
                conn.close()

        t = threading.Thread(target=_serve_one, daemon=True)
        t.start()
        try:
            client = hlp.IpcClient(port)
            try:
                reply = client.request({"cmd": "ping_py3", "val": 123})
            finally:
                client.close()
            t.join(timeout=5)
        finally:
            srv.close()

        self.assertEqual(received, [{"cmd": "ping_py3", "val": 123}])
        self.assertEqual(reply, {"ok": True, "echo": "ping_py3"})

        # 3. _RecordedBoundProxy attribute/setattr/dunder handling + _get_beyonce wrapping
        class _FakeRemotePark:
            def __init__(self):
                self.boundObject = True
                self._internal = "keep_private"
                self.warps = []

            def CmdWarpToStuff(self, *args):
                self.warps.append(args)
                return "warped"

        def _fail_bind(*_a, **_kw):
            raise RuntimeError("no bound park")

        park = _FakeRemotePark()
        sink = []
        fake_michelle = types.SimpleNamespace(
            _Michelle__bp=types.SimpleNamespace(solarsystemID=30000142),
            GetRemotePark=lambda: park,
        )
        fake_sm = types.SimpleNamespace(
            _evomu_rpc_sink=sink,
            RemoteSvc=lambda name: types.SimpleNamespace(MachoBindObject=_fail_bind),
            GetService=lambda name: fake_michelle,
        )
        proxy = hlp._get_beyonce(fake_sm, 30000142)
        self.assertEqual(proxy._internal, "keep_private")
        proxy.custom_flag = 99
        self.assertEqual(park.custom_flag, 99)
        self.assertEqual(proxy.CmdWarpToStuff("bookmark", 12345), "warped")
        self.assertEqual(park.warps, [("bookmark", 12345)])
        self.assertEqual(len(sink), 1)
        self.assertEqual(sink[0]["service"], "beyonce.bound")
        self.assertEqual(sink[0]["method"], "CmdWarpToStuff")
        self.assertEqual(sink[0]["status"], "ok")


if __name__ == "__main__":
    unittest.main()
