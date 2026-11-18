import builtins
import os
import sys
import types
import unittest

sys.modules.setdefault("__builtin__", builtins)
HARNESS_DIR = os.path.dirname(os.path.abspath(__file__))
if HARNESS_DIR not in sys.path:
    sys.path.insert(0, HARNESS_DIR)

import evomu_site


class FakeUserError(Exception):
    def __init__(self, msg, args=None):
        super().__init__(msg)
        self.msg = msg
        self.dict = args or {}

    def __repr__(self):
        return f"UserError({self.msg!r}, {self.dict!r})"


class LiveClientSyncGuardsTest(unittest.TestCase):
    def setUp(self):
        self._saved_modules = dict(sys.modules)
        self._saved_session = getattr(builtins, "session", None)
        self._saved_sm = getattr(builtins, "sm", None)
        self._saved_targets = set(evomu_site._HOOKED_CAMERA_GUARD_TARGETS)
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS.difference_update({
            "invWindow",
            "invContainer",
            "fittingSvc",
            "fittingWarnings",
            "warningIconCont",
            "hangarView",
            "baseHangarBehaviours",
            "modularHangarBehaviours",
            "modularHangarCapitalBehaviours",
            "modularHangarCamera",
            "modularHangarCapitalCamera",
            "wreck",
            "spaceMgrWreck",
            "slotsContainer",
            "sensorGfxHandler",
            "skillPlanPanel",
            "viewStateSvc",
        })

    def tearDown(self):
        for k in list(sys.modules.keys()):
            if k not in self._saved_modules:
                sys.modules.pop(k, None)
        for k, v in self._saved_modules.items():
            sys.modules[k] = v
        if self._saved_session is None:
            if hasattr(builtins, "session"):
                delattr(builtins, "session")
        else:
            builtins.session = self._saved_session
        if self._saved_sm is None:
            if hasattr(builtins, "sm"):
                delattr(builtins, "sm")
        else:
            builtins.sm = self._saved_sm
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS.clear()
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS.update(self._saved_targets)

    def test_1267_inventory_structure_invalidated_and_gauge_guarded_after_moveme_to_space(self):
        class FakeInvCache:
            def ValidateItemCanBeOpened(self, item):
                if getattr(item, "itemID", None) == 1001896944936:
                    raise FakeUserError("CustomInfo", {"info": "InventoryNotAccessible: unreachable"})
                return True

        class FakeSM:
            def GetService(self, name):
                if name == "invCache":
                    return FakeInvCache()
                raise KeyError(name)

        class FakeController:
            def __init__(self, item_id, is_structure=True):
                self.itemID = item_id
                self._is_structure = is_structure

            def GetInventoryItem(self):
                return types.SimpleNamespace(itemID=self.itemID)

            def GetItems(self):
                if self._is_structure and getattr(builtins.session, "structureid", None) is None:
                    raise FakeUserError(
                        "CustomInfo",
                        {"info": "InventoryNotAccessible: item 1001896944936 is not reachable"},
                    )
                return []

        class StructureItemsController(FakeController):
            pass

        class ShipCargoController(FakeController):
            pass

        inv_mod = types.ModuleType("eve.client.script.ui.shared.inventory.invWindow")

        def GetInvCtrlFromInvID(inv_id):
            if inv_id[0] == "StructureItems":
                return StructureItemsController(inv_id[1], is_structure=True)
            return ShipCargoController(inv_id[1], is_structure=False)

        inv_mod.GetInvCtrlFromInvID = GetInvCtrlFromInvID

        class Inventory:
            default_windowID = "Inventory"

            def __init__(self, curr_inv_id, win_id="Inventory"):
                self.currInvID = curr_inv_id
                self.default_windowID = win_id
                self.destroyed = False
                self.updateSelectedItemsPending = [1]
                self.invCont = None
                self.refresh_calls = 0

            def _IsCurrentInventoryValid(self):
                # Reproduces V24 bug: returns True when not structureid and not stationid.
                if not builtins.session.structureid and not builtins.session.stationid:
                    return True
                return True

            def RefreshTree(self, **kwargs):
                self.refresh_calls += 1

            def OnSessionChanged(self, isRemote, sess, change):
                if not self._IsCurrentInventoryValid():
                    self.currInvID = None
                self.RefreshTree(corpHangarsChanged=False)

            def _UpdateSelectedItems(self):
                raise FakeUserError(
                    "CustomInfo",
                    {"info": "InventoryNotAccessible: item 1001896944936 is not reachable"},
                )

        inv_mod.Inventory = Inventory
        sys.modules["eve.client.script.ui.shared.inventory.invWindow"] = inv_mod

        cont_mod = types.ModuleType("eve.client.script.ui.shared.container")

        class InvContCapacityGauge:
            def __init__(self, inv_cont):
                self.invCont = inv_cont
                self.destroyed = False
                self.label_updates = 0

            def SetSecondaryVolume(self, items):
                return self.UpdateLabel()

            def UpdateLabel(self):
                self.invCont.invController.GetItems()
                self.label_updates += 1
                return "ok"

        cont_mod.InvContCapacityGauge = InvContCapacityGauge
        sys.modules["eve.client.script.ui.shared.container"] = cont_mod

        builtins.sm = FakeSM()
        builtins.session = types.SimpleNamespace(
            structureid=None, stationid=None, shipid=9001, mutating=False
        )
        self.assertTrue(evomu_site._install_inventory_transition_guard())

        # Undocked structure window must report invalid and clear pending updates on session change.
        struct_wnd = Inventory(("StructureItems", 1001896944936), win_id="InventoryStructure")
        self.assertFalse(struct_wnd._IsCurrentInventoryValid())
        struct_wnd.OnSessionChanged(True, builtins.session, {"structureid": (1001896944936, None)})
        self.assertIsNone(struct_wnd.currInvID)
        self.assertIsNone(struct_wnd.updateSelectedItemsPending)

        # In-space ShipCargo inventory remains valid!
        cargo_wnd = Inventory(("ShipCargo", 9001), win_id="InventoryPrimary")
        self.assertTrue(cargo_wnd._IsCurrentInventoryValid())

        # Capacity gauge targeting unreachable structure controller absorbs cleanly without RPC crash.
        stale_cont = types.SimpleNamespace(
            destroyed=False, invController=StructureItemsController(1001896944936, is_structure=True)
        )
        gauge = InvContCapacityGauge(stale_cont)
        self.assertIsNone(gauge.SetSecondaryVolume([types.SimpleNamespace(itemID=1)]))

        # Valid ShipCargo gauge still executes UpdateLabel normally.
        valid_cont = types.SimpleNamespace(
            destroyed=False, invController=ShipCargoController(9001, is_structure=False)
        )
        valid_gauge = InvContCapacityGauge(valid_cont)
        self.assertEqual(valid_gauge.SetSecondaryVolume([]), "ok")
        self.assertEqual(valid_gauge.label_updates, 1)

    def test_1254_fitting_active_ship_none_does_not_raise_index_error(self):
        builtins.session = types.SimpleNamespace(shipid=None, stationid=60003760)
        fsvc_mod = types.ModuleType("eve.client.script.environment.fittingSvc")
        fsvc_mod.eveCfg = types.SimpleNamespace(GetActiveShip=lambda: builtins.session.shipid)

        class fittingSvc:
            def GetFittingDictForActiveShip(self, putModuleAmmoInCargo=False):
                if builtins.session.shipid is None:
                    raise IndexError("Thou shalt not send None in as itemid to GetInventoryFromId")
                return (587, [], [])

        fsvc_mod.fittingSvc = fittingSvc
        sys.modules["eve.client.script.environment.fittingSvc"] = fsvc_mod

        fw_mod = types.ModuleType("shipfitting.fittingWarnings")

        def EvaluateFit(fittingController):
            if builtins.session.shipid is None:
                raise IndexError("Thou shalt not send None in as itemid to GetInventoryFromId")
            return {"ok": True}

        fw_mod.EvaluateFit = EvaluateFit
        sys.modules["shipfitting.fittingWarnings"] = fw_mod

        wic_mod = types.ModuleType("eve.client.script.ui.shared.fittingScreen.warningIconCont")

        class WarningIconCont:
            def __init__(self):
                self.destroyed = False

            def __UpdateIcon_thread(self):
                raise IndexError("Thou shalt not send None in as itemid to GetInventoryFromId")

        wic_mod.WarningIconCont = WarningIconCont
        sys.modules["eve.client.script.ui.shared.fittingScreen.warningIconCont"] = wic_mod

        self.assertTrue(evomu_site._install_fitting_active_ship_guard())

        svc = fittingSvc()
        with self.assertRaises(RuntimeError):
            svc.GetFittingDictForActiveShip()
        self.assertIsNone(fw_mod.EvaluateFit(object()))
        self.assertIsNone(WarningIconCont()._WarningIconCont__UpdateIcon_thread())

        # Active ship present still succeeds normally.
        builtins.session.shipid = 1897500001
        self.assertEqual(svc.GetFittingDictForActiveShip(), (587, [], []))
        self.assertEqual(fw_mod.EvaluateFit(object()), {"ok": True})

    def test_1218_modular_hangar_clean_scene_handles_none_camera_and_keeps_cleanup(self):
        mhb_mod = types.ModuleType("eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours")

        class ModularHangarShipBehaviour:
            def __init__(self, cam):
                self._cam = cam
                self.smallDocks = [types.SimpleNamespace(dockedShip=1, childObject=None, dockMultiEffect=None)]
                self.bigDocks = []
                self.hangarScene = "scene"
                self.playerDock = "dock"
                self.flightPath = "path"
                self.isInitialized = True
                self.reflectionProbeThread = "thread"

            def GetCamera(self):
                return self._cam

            def CleanScene(self):
                self.GetCamera().ClearRefs()
                self.hangarScene = None
                self.playerDock = None
                self.smallDocks = None
                self.bigDocks = None
                self.flightPath = None
                self.isInitialized = False
                self.reflectionProbeThread = None

        mhb_mod.ModularHangarShipBehaviour = ModularHangarShipBehaviour
        sys.modules["eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours"] = mhb_mod

        self.assertTrue(evomu_site._install_modular_hangar_teardown_guard())

        beh = ModularHangarShipBehaviour(cam=None)
        beh.CleanScene()
        self.assertIsNone(beh.hangarScene)
        self.assertFalse(beh.isInitialized)
        self.assertIsNone(beh.GetCamera())  # Temporary instance override removed cleanly.
        # Second idempotent call while smallDocks/bigDocks are None must not crash either.
        beh.CleanScene()

    def test_1217_wreck_set_random_rotation_guards_none_model(self):
        wreck_mod = types.ModuleType("eve.client.script.environment.spaceObject.wreck")

        class Wreck:
            def __init__(self, model):
                self.model = model
                self.rot = None

            def SetRotation(self, y, p, r):
                self.rot = (y, p, r)

            def SetRandomRotation(self):
                self.SetRotation(1.0, 2.0, 3.0)
                self.model.display = 1

            def SetDungeonRotation(self, direction):
                self.SetRotation(direction[0], direction[1], direction[2])
                self.model.display = 1

            def SetBallRotation(self, rotation):
                self.model.rotationCurve = rotation
                self.model.display = 0

            def Prepare(self):
                self.SetRandomRotation()

        wreck_mod.Wreck = Wreck
        sys.modules["eve.client.script.environment.spaceObject.wreck"] = wreck_mod

        self.assertTrue(evomu_site._install_wreck_rotation_guard())

        no_model = Wreck(model=None)
        no_model.Prepare()
        self.assertEqual(no_model.rot, (1.0, 2.0, 3.0))
        no_model.SetDungeonRotation((4.0, 5.0, 6.0))
        self.assertEqual(no_model.rot, (4.0, 5.0, 6.0))
        self.assertIsNone(no_model.SetBallRotation("rotCurve"))

        # Unrelated AttributeError when model IS present must still propagate across all guarded methods.
        bad_model = Wreck(model=object())
        with self.assertRaises(AttributeError):
            bad_model.Prepare()
        with self.assertRaises(AttributeError):
            bad_model.SetRandomRotation()
        with self.assertRaises(AttributeError):
            bad_model.SetDungeonRotation((1.0, 2.0, 3.0))
        with self.assertRaises(AttributeError):
            bad_model.SetBallRotation("rotCurve")

    def test_1217_space_mgr_and_meta_hook_install_wreck_guard_on_lazy_import(self):
        space_mgr_mod = types.ModuleType("eve.client.script.parklife.spaceMgr")
        wreck_mod = types.ModuleType("eve.client.script.environment.spaceObject.wreck")

        class Wreck:
            def __init__(self, model):
                self.model = model
                self.rot = None

            def SetRandomRotation(self):
                self.rot = (7.0, 8.0, 9.0)
                self.model.display = 1

            def Prepare(self):
                self.SetRandomRotation()

        wreck_mod.Wreck = Wreck

        class SpaceMgr:
            def __init__(self):
                self.loaded = False

            def DoBallsAdded_(self, balls):
                sys.modules["eve.client.script.environment.spaceObject.wreck"] = wreck_mod
                for ball in balls:
                    self.LoadObject(ball)

            def _LoadObjects(self, balls):
                for ball in balls:
                    self.LoadObject(ball)

            def LoadObject(self, ball):
                ball.Prepare()
                self.loaded = True

        space_mgr_mod.SpaceMgr = SpaceMgr
        sys.modules["eve.client.script.parklife.spaceMgr"] = space_mgr_mod

        # Install SpaceMgr wrapper before `wreck` is imported in sys.modules.
        self.assertTrue(evomu_site._install_wreck_rotation_guard())
        self.assertIn("spaceMgrWreck", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertNotIn("wreck", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)

        scheduled = []
        sys.modules["uthread"] = types.SimpleNamespace(new=lambda fn: scheduled.append(fn))
        evomu_site._EvomuMetaHook().find_module("eve.client.script.environment.spaceObject.wreck")
        self.assertEqual(scheduled, [evomu_site._install_wreck_rotation_guard])

        # Synchronous SpaceMgr.DoBallsAdded_ -> LoadObject lazily imports wreck and guards it before Prepare().
        mgr = SpaceMgr()
        wreck = Wreck(model=None)
        mgr.DoBallsAdded_([wreck])
        self.assertTrue(mgr.loaded)
        self.assertIn("wreck", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertEqual(wreck.rot, (7.0, 8.0, 9.0))

    def test_1252_slots_container_resyncs_on_onmodulefitted_and_late_process_ship_effect(self):
        sc_mod = types.ModuleType("eve.client.script.ui.inflight.shipHud.slotsContainer")
        sc_mod.IsShipFittingFlag = lambda flag: flag in (27, 28, 29)

        class SlotsContainer:
            __notifyevents__ = ["ProcessShipEffect"]

            def __init__(self):
                self.destroyed = False
                self.modulesByID = {101: object()}
                self.init_delayed_calls = 0
                self.controller = types.SimpleNamespace(
                    GetModules=lambda: [
                        types.SimpleNamespace(itemID=101),
                        types.SimpleNamespace(itemID=102),
                    ]
                )

            def InitSlotsDelayed(self):
                self.init_delayed_calls += 1

            def ProcessShipEffect(self, godmaStm, effectState):
                return "processed"

            def OnInvChange(self, item, change):
                return "inv"

        sc_mod.SlotsContainer = SlotsContainer
        sys.modules["eve.client.script.ui.inflight.shipHud.slotsContainer"] = sc_mod

        builtins.session = types.SimpleNamespace(shipid=50001)
        self.assertTrue(evomu_site._install_slots_container_sync_guard())
        self.assertIn("OnModuleFitted", SlotsContainer.__notifyevents__)

        cont = SlotsContainer()
        cont.OnModuleFitted(90000001, 29, 17938, 481)
        self.assertEqual(cont.init_delayed_calls, 1)

        # Late ProcessShipEffect for module missing from modulesByID schedules InitSlotsDelayed.
        res = cont.ProcessShipEffect(None, types.SimpleNamespace(itemID=102))
        self.assertEqual(res, "processed")
        self.assertEqual(cont.init_delayed_calls, 2)

        # Late OnInvChange fallback for unfitted-in-HUD module on current ship triggers InitSlotsDelayed.
        cont.OnInvChange(types.SimpleNamespace(itemID=102, locationID=50001, flagID=29), {4: 0})
        self.assertEqual(cont.init_delayed_calls, 3)

    def test_1267_bf01_ship_cargo_skips_rpc_and_handles_container_errors_without_masking(self):
        class LocalUserError(Exception):
            def __init__(self, msg, err_dict=None):
                super().__init__(msg)
                self.msg = msg
                self.dict = err_dict

            def __repr__(self):
                return f"UserError({self.msg!r}, {self.dict!r})"

        validate_calls = []

        class TrackingInvCache:
            def ValidateItemCanBeOpened(self, item):
                validate_calls.append(item)
                return True

        class TrackingSM:
            def GetService(self, name):
                if name == "invCache":
                    return TrackingInvCache()
                raise KeyError(name)

        builtins.sm = TrackingSM()
        builtins.session = types.SimpleNamespace(
            structureid=None,
            stationid=None,
            solarsystemid2=30000142,
            shipid=50001,
        )

        inv_mod = types.ModuleType("eve.client.script.ui.shared.inventory.invWindow")

        class Inventory:
            def __init__(self, default_windowID, currInvID, invCont=None, use_inv_cache=False):
                self.default_windowID = default_windowID
                self.currInvID = currInvID
                self.invCont = invCont
                self._valid_exc = None
                self._valid_result = True
                self._use_inv_cache = use_inv_cache

            def _IsCurrentInventoryValid(self):
                if self._use_inv_cache:
                    return builtins.sm.GetService("invCache").ValidateItemCanBeOpened(self.invCont)
                if self._valid_exc is not None:
                    raise self._valid_exc
                return self._valid_result

            def _UpdateSelectedItems(self, *args, **kwargs):
                return None

        inv_mod.Inventory = Inventory
        sys.modules["eve.client.script.ui.shared.inventory.invWindow"] = inv_mod

        self.assertTrue(evomu_site._install_inventory_transition_guard())

        cargo_wnd = Inventory(
            default_windowID="InventoryPrimary",
            currInvID=("ShipCargo", 50001),
            invCont=types.SimpleNamespace(invID=("ShipCargo", 50001), itemID=50001),
        )
        self.assertTrue(cargo_wnd._IsCurrentInventoryValid())
        self.assertEqual(validate_calls, [])

        for err_key in (
            "InventoryNotAccessible",
            "CantOpenCargoContainer",
            "OutOfRange",
            "ContainerIsLocked",
            "ItemNotFound",
            "FakeItemNotFound",
            "NotInSameSolarSystem",
        ):
            cargo_wnd._valid_exc = LocalUserError(err_key)
            self.assertFalse(cargo_wnd._IsCurrentInventoryValid(), msg=f"direct {err_key}")
            cargo_wnd._valid_exc = LocalUserError("CustomInfo", {"info": err_key})
            self.assertFalse(cargo_wnd._IsCurrentInventoryValid(), msg=f"CustomInfo {err_key}")

        cargo_wnd._valid_exc = LocalUserError("InvalidArgument", {"info": "OutOfRange"})
        with self.assertRaises(LocalUserError) as exc_ctx:
            cargo_wnd._IsCurrentInventoryValid()
        self.assertEqual(exc_ctx.exception.msg, "InvalidArgument")

        cargo_wnd._valid_exc = KeyError("missing")
        self.assertFalse(cargo_wnd._IsCurrentInventoryValid())
        cargo_wnd._valid_exc = RuntimeError("stale")
        self.assertFalse(cargo_wnd._IsCurrentInventoryValid())

        cargo_wnd._valid_exc = ValueError("unexpected client failure")
        with self.assertRaises(ValueError):
            cargo_wnd._IsCurrentInventoryValid()
        self.assertEqual(validate_calls, [])

        # SpaceContainer delegating through orig_valid invokes ValidateItemCanBeOpened once and catches OutOfRange.
        def _failing_validate(item):
            validate_calls.append(item)
            raise LocalUserError("OutOfRange")

        builtins.sm = types.SimpleNamespace(
            GetService=lambda name: types.SimpleNamespace(ValidateItemCanBeOpened=_failing_validate)
        )
        space_item = types.SimpleNamespace(invID=("SpaceContainer", 77777), itemID=77777)
        space_wnd = Inventory(
            default_windowID="SpaceContainer",
            currInvID=("SpaceContainer", 77777),
            invCont=space_item,
            use_inv_cache=True,
        )
        self.assertFalse(space_wnd._IsCurrentInventoryValid())
        self.assertEqual(validate_calls, [space_item])

    def test_1219_hangar_clear_scene_handles_none_clear_refs_without_masking(self):
        hv_mod = types.ModuleType("eve.client.script.ui.view.hangarView")

        class HangarView:
            def __init__(self, ship_behaviour, active_scene, bad_attr=False):
                self.shipBehaviour = ship_behaviour
                self.activeHangarScene = active_scene
                self._bad_attr = bad_attr

            def ClearScene(self):
                if self._bad_attr:
                    return object().unrelated_missing_attr
                return self.shipBehaviour.CleanScene()

        hv_mod.HangarView = HangarView
        sys.modules["eve.client.script.ui.view.hangarView"] = hv_mod
        self.assertTrue(evomu_site._install_hangar_view_teardown_guard())

        # Case 1: CleanScene triggers `'NoneType' object has no attribute 'ClearRefs'` before teardown completes.
        beh_none_cam = types.SimpleNamespace(CleanScene=lambda: None.ClearRefs())
        view_clear_refs = HangarView(beh_none_cam, active_scene=object())
        self.assertIsNone(view_clear_refs.ClearScene())
        self.assertEqual(view_clear_refs._evomu_hangar_gen, 1)

        # Case 2: HangarView already torn down (shipBehaviour is None) during ClearScene.
        view_torn_down = HangarView(None, active_scene=None)
        self.assertIsNone(view_torn_down.ClearScene())
        self.assertEqual(view_torn_down._evomu_hangar_gen, 1)

        # Case 3: Unrelated AttributeError while view is active propagates untouched, and generation still increments.
        view_unrelated = HangarView(types.SimpleNamespace(CleanScene=lambda: None), active_scene=object(), bad_attr=True)
        with self.assertRaises(AttributeError):
            view_unrelated.ClearScene()
        self.assertEqual(view_unrelated._evomu_hangar_gen, 1)

        # Case 4: ModularHangarShipBehaviour.CleanScene swallows `'NoneType' object has no attribute 'ClearRefs'`
        # even if GetCamera() initially returned a non-None object, without masking unrelated AttributeError.
        mhb_mod = types.ModuleType("eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours")

        class ModularHangarShipBehaviour:
            def __init__(self, bad_attr=False):
                self._bad_attr = bad_attr

            def GetCamera(self):
                return object()

            def CleanScene(self):
                if self._bad_attr:
                    return object().unrelated_missing_attr
                None.ClearRefs()

        mhb_mod.ModularHangarShipBehaviour = ModularHangarShipBehaviour
        sys.modules["eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours"] = mhb_mod
        self.assertTrue(evomu_site._install_modular_hangar_teardown_guard())
        self.assertIsNone(ModularHangarShipBehaviour().CleanScene())
        with self.assertRaises(AttributeError):
            ModularHangarShipBehaviour(bad_attr=True).CleanScene()

    def test_1286_1287_hangar_skin_change_skipped_during_reload_and_stale_ship_switch(self):
        events = []

        def _load_dogma_item(item_id):
            events.append(("LoadItem", item_id))
            if item_id != getattr(builtins.session, "shipid", None):
                raise RuntimeError("Dogma was unable to get invItem to load dogmaItem!")

        bhb_mod = types.ModuleType("eve.client.script.ui.view.hangarBehaviours.baseHangarBehaviours")

        class BaseHangarShipBehaviour(object):
            def SetShipDamage(self, itemID, model, steps=1, duration=0.1):
                events.append(("SetShipDamage", itemID, model))
                _load_dogma_item(itemID)
                return True

        bhb_mod.BaseHangarShipBehaviour = BaseHangarShipBehaviour
        sys.modules["eve.client.script.ui.view.hangarBehaviours.baseHangarBehaviours"] = bhb_mod

        mhb_mod = types.ModuleType("eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours")

        class ModularHangarShipBehaviour(BaseHangarShipBehaviour):
            def __init__(self, item_id=0, type_id=0):
                self.playerShipItemID = item_id
                self.playerShipTypeID = type_id

            def UpdateActiveShipIDs(self, type_id, item_id):
                self.playerShipTypeID = type_id
                self.playerShipItemID = item_id

            def CleanScene(self):
                events.append("CleanScene")

            def ShouldSwitchSkin(self, _skin_id):
                return True

            def LoadShipModel(self, item_id, type_id, playerShip=False, skipSetupAnimation=False):
                events.append(("Modular.LoadShipModel", item_id, type_id, playerShip))
                if playerShip:
                    self.UpdateActiveShipIDs(type_id, item_id)
                return ("model", item_id, type_id)

            def ProcessSkinChange(self, new_model):
                events.append(("Modular.ProcessSkinChange", self.playerShipItemID, new_model))
                return self.SetShipDamage(self.playerShipItemID, new_model)

        mhb_mod.ModularHangarShipBehaviour = ModularHangarShipBehaviour
        sys.modules["eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours"] = mhb_mod

        mcb_mod = types.ModuleType("eve.client.script.ui.view.hangarBehaviours.modularHangarCapitalBehaviours")

        class ModularCapitalHangarShipBehaviour(BaseHangarShipBehaviour):
            def __init__(self, item_id=0, type_id=0):
                self.playerShipItemID = item_id
                self.playerShipTypeID = type_id

            def LoadShipModel(self, item_id, type_id, playerShip=True):
                events.append(("ModularCap.LoadShipModel", item_id, type_id, playerShip))
                if playerShip:
                    self.playerShipItemID = item_id
                    self.playerShipTypeID = type_id
                return ("cap_model", item_id, type_id)

            def ProcessSkinChange(self, new_model):
                events.append(("ModularCap.ProcessSkinChange", self.playerShipItemID, new_model))
                return self.SetShipDamage(self.playerShipItemID, new_model)

        mcb_mod.ModularCapitalHangarShipBehaviour = ModularCapitalHangarShipBehaviour
        sys.modules["eve.client.script.ui.view.hangarBehaviours.modularHangarCapitalBehaviours"] = mcb_mod

        hv_mod = types.ModuleType("eve.client.script.ui.view.hangarView")

        class HangarView(object):
            def __init__(self, behaviour, on_fade=None):
                self.shipBehaviour = behaviour
                self.activeHangarScene = object()
                self.activeShipModel = ("old_model", getattr(behaviour, "playerShipItemID", 0))
                self.activeShipItem = types.SimpleNamespace(itemID=getattr(behaviour, "playerShipItemID", 0))
                self._on_fade = on_fade
                self._raise_in_reload = False

            def ClearScene(self):
                events.append("HangarView.ClearScene")
                if self.shipBehaviour is not None:
                    self.shipBehaviour.CleanScene()
                self.shipBehaviour = None
                self.activeHangarScene = None
                self.activeShipModel = None
                self.activeShipItem = None

            def ReloadView(self):
                events.append("HangarView.ReloadView.enter")
                if self._on_fade is not None:
                    self._on_fade(self)
                if self._raise_in_reload:
                    raise RuntimeError("reload_failed")
                self.ClearScene()
                # Recreate fresh capital hangar state after ClearScene (simulates SetUpModels + PlaceShip).
                self.shipBehaviour = ModularCapitalHangarShipBehaviour(0, 0)
                self.activeHangarScene = object()
                self.activeShipItem = types.SimpleNamespace(itemID=builtins.session.shipid)
                self.activeShipModel = ("cap_model", builtins.session.shipid)
                self.shipBehaviour.playerShipItemID = builtins.session.shipid
                self.shipBehaviour.playerShipTypeID = 11567

            def ProcessActiveShipChanged(self, ship_id, _old_ship_id):
                self.activeShipItem = types.SimpleNamespace(itemID=ship_id)
                return self.ReloadView()

            def OnActiveShipSkinChange(self, item_id, _skin_id, updateSkin=True, reloadSubsystems=False):
                events.append(("OnActiveShipSkinChange.orig", item_id, updateSkin, reloadSubsystems))
                new_model = self.shipBehaviour.LoadShipModel(item_id, 11567)
                return self.shipBehaviour.ProcessSkinChange(new_model)

        hv_mod.HangarView = HangarView
        sys.modules["eve.client.script.ui.view.hangarView"] = hv_mod

        evomu_site._install_live_client_sync_guards()

        # Case 1: Exact #1286/#1287 race: docked Rupture (1897286922) switches to Avatar (1897140648),
        # FadeIn yields BEFORE ClearScene while FittingController triggers OnActiveShipSkinChange(1897140648).
        builtins.session = types.SimpleNamespace(stationid=None, structureid=1001896944936, shipid=1897140648)

        def _trigger_skin_mid_fade(hv):
            self.assertTrue(getattr(hv, "_evomu_hangar_reload_pending", False))
            self.assertIsNone(hv.OnActiveShipSkinChange(1897140648, None, updateSkin=True, reloadSubsystems=True))

        view_reload = HangarView(ModularHangarShipBehaviour(1897286922, 629), on_fade=_trigger_skin_mid_fade)
        view_reload.ProcessActiveShipChanged(1897140648, 1897286922)
        self.assertFalse(getattr(view_reload, "_evomu_hangar_reload_pending", False))
        self.assertEqual(view_reload._evomu_hangar_gen, 1)
        self.assertEqual(
            [e for e in events if e[0] in ("OnActiveShipSkinChange.orig", "LoadItem", "Modular.ProcessSkinChange")],
            [],
        )
        # Once reload + PlaceShip finishes, skin change on the active Avatar runs normally.
        self.assertTrue(view_reload.OnActiveShipSkinChange(1897140648, 77))

        # Case 2: Same-hangar switch async window (0.2s / boarding moment without ReloadView):
        # activeShipItem already updated to 1897140648, but playerShipItemID is still old 1897286922.
        events[:] = []
        beh_same = ModularHangarShipBehaviour(1897286922, 629)
        view_same = HangarView(beh_same)
        view_same.activeShipItem = types.SimpleNamespace(itemID=1897140648)
        self.assertIsNone(view_same.OnActiveShipSkinChange(1897140648, None, updateSkin=True, reloadSubsystems=True))
        self.assertIsNone(view_same.OnActiveShipSkinChange(1897286922, 5))
        self.assertEqual(events, [])
        beh_same.UpdateActiveShipIDs(11567, 1897140648)
        self.assertTrue(view_same.OnActiveShipSkinChange(1897140648, 99))
        self.assertIn(("LoadItem", 1897140648), events)

        # Case 3: Non-modular shipBehaviour (no playerShipItemID) is still skipped mid-ReloadView,
        # and an exception in ReloadView resets _evomu_hangar_reload_pending cleanly.
        events[:] = []

        class PlainHangarBehaviour(BaseHangarShipBehaviour):
            def LoadShipModel(self, item_id, type_id):
                events.append(("Plain.LoadShipModel", item_id, type_id))
                return ("plain", item_id)

            def ProcessSkinChange(self, new_model):
                return self.SetShipDamage(builtins.session.shipid, new_model)

        view_plain = HangarView(PlainHangarBehaviour(), on_fade=_trigger_skin_mid_fade)
        view_plain._raise_in_reload = True
        with self.assertRaises(RuntimeError):
            view_plain.ReloadView()
        self.assertFalse(getattr(view_plain, "_evomu_hangar_reload_pending", False))
        self.assertEqual(events, ["HangarView.ReloadView.enter"])

        # Case 4: Defense-in-depth on ProcessSkinChange & SetShipDamage + unmasked genuine errors.
        events[:] = []
        self.assertIsNone(ModularHangarShipBehaviour(0, 0).ProcessSkinChange(object()))
        self.assertIsNone(ModularHangarShipBehaviour(1897286922, 629).ProcessSkinChange(object()))
        self.assertIsNone(ModularCapitalHangarShipBehaviour(0, 0).ProcessSkinChange(object()))
        self.assertIsNone(ModularCapitalHangarShipBehaviour(1897286922, 11567).ProcessSkinChange(object()))
        self.assertFalse(BaseHangarShipBehaviour().SetShipDamage(0, object()))
        self.assertFalse(BaseHangarShipBehaviour().SetShipDamage(1897286922, object()))
        self.assertEqual(events, [])

        # Undocked SetShipDamage returns False cleanly without LoadItem.
        builtins.session = types.SimpleNamespace(stationid=None, structureid=None, shipid=1897140648)
        self.assertFalse(BaseHangarShipBehaviour().SetShipDamage(1897140648, object()))
        self.assertEqual(events, [])

        # Genuine RuntimeError on the active docked ship still propagates (never masked).
        builtins.session = types.SimpleNamespace(stationid=None, structureid=1001896944936, shipid=1897140648)
        beh_cap = ModularCapitalHangarShipBehaviour(1897140648, 11567)

        def _broken_set_ship_damage(_item_id, _model):
            raise RuntimeError("genuine_dogma_bug_on_active_ship")

        beh_cap.SetShipDamage = _broken_set_ship_damage
        with self.assertRaises(RuntimeError):
            beh_cap.ProcessSkinChange(object())

    def test_1286_late_imported_modular_capital_and_base_behaviours_still_hooked(self):
        # Simulate modularHangarBehaviours importing before baseHangarBehaviours / modularHangarCapitalBehaviours.
        mhb_mod = types.ModuleType("eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours")

        class ModularHangarShipBehaviour(object):
            def CleanScene(self):
                return None

            def ProcessSkinChange(self, _model):
                return "should_not_run"

        mhb_mod.ModularHangarShipBehaviour = ModularHangarShipBehaviour
        sys.modules["eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours"] = mhb_mod
        evomu_site._install_live_client_sync_guards()
        self.assertIn("modularHangarBehaviours", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertNotIn("modularHangarCapitalBehaviours", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertNotIn("baseHangarBehaviours", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)

        bhb_mod = types.ModuleType("eve.client.script.ui.view.hangarBehaviours.baseHangarBehaviours")

        class BaseHangarShipBehaviour(object):
            def SetShipDamage(self, _item_id, _model):
                return True

        bhb_mod.BaseHangarShipBehaviour = BaseHangarShipBehaviour
        sys.modules["eve.client.script.ui.view.hangarBehaviours.baseHangarBehaviours"] = bhb_mod

        mcb_mod = types.ModuleType("eve.client.script.ui.view.hangarBehaviours.modularHangarCapitalBehaviours")

        class ModularCapitalHangarShipBehaviour(object):
            def __init__(self, ship_id):
                self.playerShipItemID = ship_id

            def ProcessSkinChange(self, _model):
                return "should_not_run"

        mcb_mod.ModularCapitalHangarShipBehaviour = ModularCapitalHangarShipBehaviour
        sys.modules["eve.client.script.ui.view.hangarBehaviours.modularHangarCapitalBehaviours"] = mcb_mod

        evomu_site._EvomuMetaHook().find_module("late")
        self.assertIn("baseHangarBehaviours", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertIn("modularHangarCapitalBehaviours", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)

        builtins.session = types.SimpleNamespace(stationid=60003760, structureid=None, shipid=200)
        self.assertFalse(BaseHangarShipBehaviour().SetShipDamage(100, object()))
        self.assertIsNone(ModularCapitalHangarShipBehaviour(100).ProcessSkinChange(object()))

    def _make_fake_gfxhandler_cls(self, recycle_fn):
        class GfxHandler(object):
            def __init__(self, scene_mgr):
                self.sceneManager = scene_mgr
                self.gfxSensorSwipe = None
                self.gfxSwipeThread = "active-thread"

            def StartGfxSwipe(self, viewAngleInPlane):
                if self.gfxSensorSwipe is None:
                    self.gfxSensorSwipe = recycle_fn("res:/fisfx/scanner/background.red")
                return ("started", viewAngleInPlane, self.gfxSensorSwipe)

            def _GfxSwipeThread(self, viewAngleInPlane):
                try:
                    return self.StartGfxSwipe(viewAngleInPlane)
                finally:
                    self.gfxSwipeThread = None

        return GfxHandler

    def test_1292_sensor_gfx_swipe_happy_path_and_transition_skips(self):
        recycle_calls = []

        def recycle(path):
            recycle_calls.append(path)
            return {"loaded": path}

        gfx_mod = types.ModuleType("sensorsuite.overlay.gfxhandler")
        GfxHandler = self._make_fake_gfxhandler_cls(recycle)
        gfx_mod.GfxHandler = GfxHandler
        sys.modules["sensorsuite.overlay.gfxhandler"] = gfx_mod

        self.assertTrue(evomu_site._install_sensor_suite_gfx_guard())

        in_jump = [False]
        builtins.sm = types.SimpleNamespace(
            IsServiceRunning=lambda name: name == "subway",
            GetService=lambda name: types.SimpleNamespace(InJump=lambda: in_jump[0]),
        )
        default_scene = [object()]
        scene_mgr = types.SimpleNamespace(GetRegisteredScene=lambda name: default_scene[0] if name == "default" else None)

        # Valid in-space state executes StartGfxSwipe and _GfxSwipeThread normally.
        builtins.session = types.SimpleNamespace(solarsystemid=30000144, stationid=None, structureid=None, mutating=False)
        handler = GfxHandler(scene_mgr)
        res = handler._GfxSwipeThread(-2.906)
        self.assertEqual(res, ("started", -2.906, {"loaded": "res:/fisfx/scanner/background.red"}))
        self.assertIsNone(handler.gfxSwipeThread)
        self.assertEqual(len(recycle_calls), 1)

        # Each jump / dock / teardown / missing-scene condition skips BEFORE calling RecycleOrLoad.
        blocked_cases = [
            (types.SimpleNamespace(solarsystemid=None, stationid=None, structureid=None, mutating=False), False, object(), scene_mgr),
            (types.SimpleNamespace(solarsystemid=30000144, stationid=60003760, structureid=None, mutating=False), False, object(), scene_mgr),
            (types.SimpleNamespace(solarsystemid=30000144, stationid=None, structureid=1001, mutating=False), False, object(), scene_mgr),
            (types.SimpleNamespace(solarsystemid=30000144, stationid=None, structureid=None, mutating=True), False, object(), scene_mgr),
            (types.SimpleNamespace(solarsystemid=30000144, stationid=None, structureid=None, mutating=False), True, object(), scene_mgr),
            (types.SimpleNamespace(solarsystemid=30000144, stationid=None, structureid=None, mutating=False), False, None, scene_mgr),
            (types.SimpleNamespace(solarsystemid=30000144, stationid=None, structureid=None, mutating=False), False, None, None),
        ]
        for sess, jumping, scene_obj, mgr in blocked_cases:
            builtins.session = sess
            in_jump[0] = jumping
            default_scene[0] = scene_obj
            h = GfxHandler(mgr)
            self.assertIsNone(h.StartGfxSwipe(-1.5))
            self.assertIsNone(h._GfxSwipeThread(-1.5))
            self.assertIsNone(h.gfxSensorSwipe)
            self.assertIsNone(h.gfxSwipeThread)
        self.assertEqual(len(recycle_calls), 1)

    def test_1292_1293_sensor_gfx_swipe_absorbs_couldnt_get_object_when_none_and_propagates_real_errors(self):
        exc_to_raise = [RuntimeError("Couldn't get object")]

        def fake_recycle(_path):
            raise exc_to_raise[0]

        gfx_mod = types.ModuleType("sensorsuite.overlay.gfxhandler")

        class GfxHandler(object):
            def __init__(self):
                self.sceneManager = types.SimpleNamespace(GetRegisteredScene=lambda _name: object())
                self.gfxSensorSwipe = None
                self.gfxSwipeThread = "active-thread"
                self.loaded_err = None

            def StartGfxSwipe(self, _viewAngleInPlane):
                if self.gfxSensorSwipe is None:
                    self.gfxSensorSwipe = fake_recycle("res:/fisfx/scanner/background.red")
                if self.loaded_err is not None:
                    raise self.loaded_err

            def _GfxSwipeThread(self, viewAngleInPlane):
                return self.StartGfxSwipe(viewAngleInPlane)

            def _DirectRecycleInThread(self):
                return fake_recycle("res:/fisfx/scanner/background.red")

        gfx_mod.GfxHandler = GfxHandler
        sys.modules["sensorsuite.overlay.gfxhandler"] = gfx_mod
        self.assertTrue(evomu_site._install_sensor_suite_gfx_guard())

        builtins.sm = types.SimpleNamespace(IsServiceRunning=lambda _name: False)
        builtins.session = types.SimpleNamespace(solarsystemid=30000144, stationid=None, structureid=None, mutating=False)

        # Mid-reload RuntimeError("Couldn't get object") when gfxSensorSwipe is None is absorbed cleanly.
        h = GfxHandler()
        self.assertIsNone(h.StartGfxSwipe(-2.9))
        self.assertIsNone(h._GfxSwipeThread(-2.9))
        self.assertIsNone(h.gfxSensorSwipe)
        self.assertIsNone(h.gfxSwipeThread)

        # Direct wrapper test on _GfxSwipeThread in case an error surfaces inside the thread body itself.
        h_thread_direct = GfxHandler()
        direct_wrapped = evomu_site._make_sensor_gfx_swipe_thread_guard(GfxHandler._DirectRecycleInThread)
        self.assertIsNone(direct_wrapped(h_thread_direct))
        self.assertIsNone(h_thread_direct.gfxSensorSwipe)
        self.assertIsNone(h_thread_direct.gfxSwipeThread)

        # Unrelated RuntimeError when gfxSensorSwipe is None must still propagate.
        exc_to_raise[0] = RuntimeError("unexpected gfx failure")
        with self.assertRaises(RuntimeError):
            GfxHandler().StartGfxSwipe(0.0)
        with self.assertRaises(RuntimeError):
            GfxHandler()._GfxSwipeThread(0.0)

        # Non-RuntimeError must still propagate even if message resembles recycler error.
        exc_to_raise[0] = ValueError("Couldn't get object")
        with self.assertRaises(ValueError):
            GfxHandler().StartGfxSwipe(0.0)

        # If gfxSensorSwipe was already populated, RuntimeError("Couldn't get object") still propagates.
        h_loaded = GfxHandler()
        h_loaded.gfxSensorSwipe = object()
        h_loaded.loaded_err = RuntimeError("Couldn't get object")
        with self.assertRaises(RuntimeError):
            h_loaded.StartGfxSwipe(0.0)
        with self.assertRaises(RuntimeError):
            h_loaded._GfxSwipeThread(0.0)

    def test_1292_late_import_via_meta_hook_guards_sensor_gfx_handler(self):
        evomu_site._install_live_client_sync_guards()
        self.assertNotIn("sensorGfxHandler", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)

        def fail_recycle(_path):
            raise RuntimeError("Couldn't get object")

        gfx_mod = types.ModuleType("sensorsuite.overlay.gfxhandler")
        gfx_mod.GfxHandler = self._make_fake_gfxhandler_cls(fail_recycle)
        sys.modules["sensorsuite.overlay.gfxhandler"] = gfx_mod

        evomu_site._EvomuMetaHook().find_module("sensorsuite.overlay.gfxhandler")
        self.assertIn("sensorGfxHandler", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)

        builtins.session = types.SimpleNamespace(solarsystemid=30000144, stationid=None, structureid=None, mutating=False)
        builtins.sm = types.SimpleNamespace(IsServiceRunning=lambda _name: False)
        handler = gfx_mod.GfxHandler(types.SimpleNamespace(GetRegisteredScene=lambda _n: object()))
        self.assertIsNone(handler._GfxSwipeThread(-2.906))
        self.assertIsNone(handler.gfxSensorSwipe)
        self.assertIsNone(handler.gfxSwipeThread)

    def test_1218_1254_live_client_chains_regression_verification(self):
        # #1218: ModularCapitalHangarShipBehaviour inherits ModularHangarShipBehaviour.CleanScene and GetCamera()->None.
        mhb_mod = types.ModuleType("eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours")

        class ModularHangarShipBehaviour(object):
            def __init__(self):
                self.smallDocks = []
                self.bigDocks = []
                self.hangarScene = "scene"
                self.isInitialized = True
                self.cam_obj = None
                self.raise_unrelated = False

            def GetCamera(self):
                return self.cam_obj

            def CleanScene(self):
                self.GetCamera().ClearRefs()
                if self.raise_unrelated:
                    raise AttributeError("unexpected cleanup failure")
                self.hangarScene = None
                self.isInitialized = False

        class ModularCapitalHangarShipBehaviour(ModularHangarShipBehaviour):
            def GetCamera(self):
                return self.cam_obj

        mhb_mod.ModularHangarShipBehaviour = ModularHangarShipBehaviour
        sys.modules["eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours"] = mhb_mod

        hv_mod = types.ModuleType("eve.client.script.ui.view.hangarView")

        class HangarView(object):
            def __init__(self, behaviour):
                self.shipBehaviour = behaviour

            def ClearScene(self):
                self.shipBehaviour.CleanScene()
                self.shipBehaviour = None

        hv_mod.HangarView = HangarView
        sys.modules["eve.client.script.ui.view.hangarView"] = hv_mod

        self.assertTrue(evomu_site._install_hangar_view_teardown_guard())
        cap_beh = ModularCapitalHangarShipBehaviour()
        view = HangarView(cap_beh)
        view.ClearScene()
        self.assertIsNone(view.shipBehaviour)
        self.assertIsNone(cap_beh.hangarScene)
        self.assertFalse(cap_beh.isInitialized)

        # Unrelated AttributeError on a valid camera still propagates.
        valid_beh = ModularCapitalHangarShipBehaviour()
        valid_beh.cam_obj = types.SimpleNamespace(ClearRefs=lambda: None)
        valid_beh.raise_unrelated = True
        with self.assertRaises(AttributeError):
            valid_beh.CleanScene()

        # #1254: Real EvaluateFit -> GetFittingDictForActiveShip chain returns None on shipid=None without invCache(None).
        builtins.session = types.SimpleNamespace(shipid=None, stationid=60003760)
        inv_calls = []
        fsvc_mod = types.ModuleType("eve.client.script.environment.fittingSvc")
        fsvc_mod.eveCfg = types.SimpleNamespace(GetActiveShip=lambda: builtins.session.shipid)

        class fittingSvc(object):
            def GetFittingDictForActiveShip(self, putModuleAmmoInCargo=False):
                inv_calls.append(fsvc_mod.eveCfg.GetActiveShip())
                if inv_calls[-1] is None:
                    raise IndexError("Thou shalt not send None in as itemid to GetInventoryFromId")
                return (587, [], [])

        fsvc_mod.fittingSvc = fittingSvc
        sys.modules["eve.client.script.environment.fittingSvc"] = fsvc_mod

        svc_inst = fittingSvc()
        fw_mod = types.ModuleType("shipfitting.fittingWarnings")

        def EvaluateFit(_fittingController):
            try:
                fit = svc_inst.GetFittingDictForActiveShip()
            except (RuntimeError, AttributeError):
                return None
            return {"ship": fit[0]}

        fw_mod.EvaluateFit = EvaluateFit
        sys.modules["shipfitting.fittingWarnings"] = fw_mod

        self.assertTrue(evomu_site._install_fitting_active_ship_guard())
        self.assertIsNone(fw_mod.EvaluateFit(object()))
        self.assertEqual(inv_calls, [])
        builtins.session.shipid = 1897500001
        self.assertEqual(fw_mod.EvaluateFit(object()), {"ship": 587})
        self.assertEqual(inv_calls, [1897500001])

    def test_1290_1301_move_items_user_error_shows_dialog_without_log_exception_or_masking(self):
        class UserError(Exception):
            def __init__(self, msg, args=None):
                Exception.__init__(self, msg, args or {})
                self.msg = msg
                self.dict = args or {}

            def __repr__(self):
                return f"UserError({self.msg!r}, {self.dict!r})"

        class ExactUserError(Exception):
            def __init__(self, msg, args=None):
                super().__init__(msg, args or {})
                self.msg = msg
                self.dict = args or {}

        eve_exc_mod = types.ModuleType("eveexceptions")
        eve_exc_mod.UserError = ExactUserError
        sys.modules["eveexceptions"] = eve_exc_mod

        shown_dialogs = []
        ui_msg_mod = types.ModuleType("player_messaging.client.ui_message")

        def prompt_player_fsd_dialog(msg, err_dict=None):
            shown_dialogs.append(("fsd", msg, err_dict))

        ui_msg_mod.prompt_player_fsd_dialog = prompt_player_fsd_dialog
        sys.modules["player_messaging.client.ui_message"] = ui_msg_mod

        log_mod_calls = []
        log_mod = types.ModuleType("logmodule")
        log_mod.uiMessageFunc = lambda msg, err_dict=None: shown_dialogs.append(("logui", msg, err_dict))
        log_mod.LogException = lambda *a, **kw: log_mod_calls.append((a, kw))
        sys.modules["logmodule"] = log_mod

        inv_mod = types.ModuleType("eve.client.script.ui.shared.inventory.invWindow")
        inv_mod.prompt_player_fsd_dialog = prompt_player_fsd_dialog

        class Inventory:
            def _IsCurrentInventoryValid(self):
                return True

            def _MoveItems(self, entry, nodes):
                entry.data.invController.OnDropData(nodes)
                entry.Blink()
                return "moved"

        inv_mod.Inventory = Inventory
        sys.modules["eve.client.script.ui.shared.inventory.invWindow"] = inv_mod
        self.assertTrue(evomu_site._install_inventory_transition_guard())

        wnd = Inventory()
        blinks = []

        class Controller:
            def __init__(self, mode):
                self.mode = mode

            def OnDropData(self, _nodes):
                if self.mode == "avatar_1290":
                    raise ExactUserError("NotEnoughCargoSpace", {"volume": 750.0, "available": 0.0})
                if self.mode == "venture_1301":
                    raise UserError("NotEnoughCargoSpace", {"volume": 1.6, "available": 0.0})
                if self.mode == "runtime_bug":
                    raise RuntimeError("genuine_move_items_bug")
                return True

        def mk_entry(mode):
            return types.SimpleNamespace(
                data=types.SimpleNamespace(invController=Controller(mode)),
                Blink=lambda: blinks.append(mode),
            )

        # #1290 & #1301 overfilled cargo NotEnoughCargoSpace UserErrors must display dialog cleanly without LogException.
        self.assertIsNone(wnd._MoveItems(mk_entry("avatar_1290"), [object()]))
        self.assertIsNone(wnd._MoveItems(mk_entry("venture_1301"), [object()]))
        self.assertEqual(
            shown_dialogs,
            [
                ("fsd", "NotEnoughCargoSpace", {"volume": 750.0, "available": 0.0}),
                ("fsd", "NotEnoughCargoSpace", {"volume": 1.6, "available": 0.0}),
            ],
        )
        self.assertEqual(log_mod_calls, [])
        self.assertEqual(blinks, [])

        # Fallback to logmodule.uiMessageFunc works when prompt_player_fsd_dialog raises or is unavailable.
        def failing_fsd_dialog(_msg, _err_dict=None):
            raise KeyError("missing_static_dialog")

        inv_mod.prompt_player_fsd_dialog = failing_fsd_dialog
        ui_msg_mod.prompt_player_fsd_dialog = failing_fsd_dialog
        self.assertIsNone(wnd._MoveItems(mk_entry("avatar_1290"), [object()]))
        self.assertEqual(
            shown_dialogs[-1],
            ("logui", "NotEnoughCargoSpace", {"volume": 750.0, "available": 0.0}),
        )

        # Fallback also reaches eve.Message when logmodule.uiMessageFunc fails, and recovers when primary is absent.
        eve_calls = []
        builtins.eve = types.SimpleNamespace(
            Message=lambda msg, err_dict=None: eve_calls.append((msg, err_dict))
        )
        orig_ui_msg = log_mod.uiMessageFunc
        try:
            log_mod.uiMessageFunc = failing_fsd_dialog
            self.assertIsNone(wnd._MoveItems(mk_entry("venture_1301"), [object()]))
            self.assertEqual(
                eve_calls,
                [("NotEnoughCargoSpace", {"volume": 1.6, "available": 0.0})],
            )
        finally:
            log_mod.uiMessageFunc = orig_ui_msg
            delattr(builtins, "eve")

        delattr(inv_mod, "prompt_player_fsd_dialog")
        sys.modules.pop("player_messaging.client.ui_message", None)
        self.assertIsNone(wnd._MoveItems(mk_entry("avatar_1290"), [object()]))
        self.assertEqual(
            shown_dialogs[-1],
            ("logui", "NotEnoughCargoSpace", {"volume": 750.0, "available": 0.0}),
        )

        # When every dialog presenter fails/is unavailable, UserError must re-raise rather than silently drop.
        log_mod.uiMessageFunc = failing_fsd_dialog
        try:
            with self.assertRaises(ExactUserError):
                wnd._MoveItems(mk_entry("avatar_1290"), [object()])
        finally:
            log_mod.uiMessageFunc = orig_ui_msg

        # Genuine non-UserError exceptions must propagate unmasked.
        with self.assertRaises(RuntimeError):
            wnd._MoveItems(mk_entry("runtime_bug"), [object()])

        # Successful drop passes through normally and calls entry.Blink().
        self.assertEqual(wnd._MoveItems(mk_entry("ok"), [object()]), "moved")
        self.assertEqual(blinks, ["ok"])

    def test_1318_modular_hangar_camera_none_model_secondary_view_reopen_and_fade_recovery(self):
        events = []
        delayed_calls = []

        uthread2_mod = types.ModuleType("uthread2")

        def _fake_delay(fn, delay, *args, **kwargs):
            delayed_calls.append((delay, fn, args, kwargs))
            return ("timer", len(delayed_calls))

        uthread2_mod.call_after_simtime_delay = _fake_delay
        sys.modules["uthread2"] = uthread2_mod

        mhc_mod = types.ModuleType("eve.client.script.ui.camera.modularHangarCamera")

        class ModularHangarCamera(object):
            def __init__(self):
                self.model = None
                self.currentShipSizeClass = 0
                self._atPosition = (0.0, 0.0, 0.0)
                self._eyePosition = (-354.38, -1148.97, -680.02)
                self.atPosition = self._atPosition
                self.changingShip = False
                self.yaw = 0.5
                self.pitch = 0.5
                self.zoomFactor = 0.5

            def GetZoomToPoint(self):
                return self.atPosition

            def GetEyePosition(self):
                return self._eyePosition

            def SetEyePosition(self, pos):
                self._eyePosition = pos
                events.append(("SetEyePosition", pos))

            def _CalculateFocalPoint(self, model):
                return (0.0, 0.0, 0.0) if model is None else (1.0, 2.0, 3.0)

            def GetSizeDependantZoomConstants(self):
                axis = max(self.model.generatedShapeEllipsoidRadius)
                if self.currentShipSizeClass == 2:
                    return (1450.0 + 0.2 * axis, 250.0, 500.0)
                return (385.0 + 0.4 * axis, 10.0, 120.0)

            def CalculateTargetPoint(self):
                _ = max(self.model.generatedShapeEllipsoidRadius)
                self.GetSizeDependantZoomConstants()
                _ = self.model.rotationCurve.currentValue
                return (10.0, 20.0, 30.0)

            def _updateShipChangeLocations(self):
                _ = max(self.model.generatedShapeEllipsoidRadius)
                return "updated"

            def AnimEnterHangar(self, model, startPos, endPos, duration=None):
                self.model = model
                self.atPosition = self._CalculateFocalPoint(model)
                self.changingShip = False
                self.SetEyePosition(self.CalculateTargetPoint())
                self.yaw = 0
                self.pitch = 0

                def AllignCamera():
                    _ = model.rotationCurve.currentValue
                    self.yaw = 1.2
                    events.append("AllignCamera")

                uthread2_mod.call_after_simtime_delay(AllignCamera, 0.3)

        mhc_mod.ModularHangarCamera = ModularHangarCamera
        sys.modules["eve.client.script.ui.camera.modularHangarCamera"] = mhc_mod

        hv_mod = types.ModuleType("eve.client.script.ui.view.hangarView")
        hv_mod.REPLACE_UPDATE = 2
        hv_mod.REPLACE_SWITCHSHIPS = 1

        class ModularHangarShipBehaviour(object):
            def __init__(self, ship_to_place=None, small_docks=None, repopulate_dock=False):
                self.smallDocks = [] if small_docks is None else list(small_docks)
                self.bigDocks = []
                self.ship_to_place = ship_to_place
                self.repopulate_dock = repopulate_dock
                self.playerDock = None

            def SetAnchorPoint(self, hangar_model, hangar_scene):
                events.append(("SetAnchorPoint", hangar_model, hangar_scene))
                if self.repopulate_dock:
                    self.smallDocks.append("dock_1")

            def UpdateActiveShipIDs(self, type_id, item_id):
                events.append(("UpdateActiveShipIDs", type_id, item_id))

            def populateDocksWithInventoryShips(self, ships, scene, hangar):
                events.append("populateDocksWithInventoryShips")

            def PlaceShip(self, item_id, type_id, skip_anim):
                events.append(("PlaceShip", item_id, type_id, bool(self.smallDocks)))
                if not self.smallDocks:
                    return None
                return self.ship_to_place

        class HangarView(object):
            def __init__(self, ship_behaviour, cam):
                self.shipBehaviour = ship_behaviour
                self.activeHangarScene = types.SimpleNamespace(objects=[])
                self.activeHangarModel = object()
                self.activeShipModel = None
                self.skipDockingAnimation = False
                self._starting_boarding_moment = False
                self._retriggerableFadeFromBlackTasklet = None
                self.invCache = None
                self._cam = cam

            def GetCamera(self):
                return self._cam

            def GetActiveShipItemID(self):
                return 1897518389

            def GetActiveShipTypeID(self):
                return 32880

            def _FadeFromBlackHandler(self, delay, is_quick):
                events.append(("_FadeFromBlackHandler", delay, is_quick))
                self._retriggerableFadeFromBlackTasklet = ("fade", delay, is_quick)

            def ClearScene(self):
                self.shipBehaviour = None
                self.activeHangarScene = None
                self.activeShipModel = None

            def LoadCamera(self, cameraID=None, reopen=False):
                if not reopen or self.activeShipModel is None:
                    self.ReplaceExistingShipModel(hv_mod.REPLACE_UPDATE, fade=True)

            def ReplaceExistingShipModel(self, eventType, fade=True, skipAnimation=False):
                events.append(("ReplaceExistingShipModel", eventType, fade, skipAnimation))
                item_id = self.GetActiveShipItemID()
                type_id = self.GetActiveShipTypeID()
                self._ReplaceExistingShipModelModular(eventType, item_id, type_id, skipAnimation)

            def _ReplaceExistingShipModelModular(self, eventType, itemID, typeID, skipAnimation):
                self.shipBehaviour.UpdateActiveShipIDs(self.GetActiveShipTypeID(), self.GetActiveShipItemID())
                self.shipBehaviour.populateDocksWithInventoryShips([], self.activeHangarScene, self.activeHangarModel)
                ship = self.shipBehaviour.PlaceShip(itemID, typeID, self.skipDockingAnimation)
                if ship is not None and self.activeShipModel is not None and ship.name == self.activeShipModel.name:
                    self.activeShipModel = ship
                    return
                self.GetCamera().AnimEnterHangar(ship, None, None)
                if not self._starting_boarding_moment:
                    self._FadeFromBlackHandler(0.5, False)
                self.activeShipModel = ship

        hv_mod.HangarView = HangarView
        sys.modules["eve.client.script.ui.view.hangarView"] = hv_mod

        vss_mod = types.ModuleType("eve.client.script.ui.services.viewStateSvc")

        class ViewStateSvc(object):
            def __init__(self, hangar_view):
                self.hangar_view = hangar_view

            def _OpenView(self, viewInfo, reopen=False, cameraID=None, **kwargs):
                self.hangar_view.LoadCamera(cameraID=cameraID, reopen=reopen)

            def _OpenPrimaryView(self, name, **kwargs):
                self._OpenView(types.SimpleNamespace(name=name), reopen=True)

            def ActivateView(self, name, **kwargs):
                self._OpenPrimaryView(name, **kwargs)

            def CloseSecondaryView(self):
                self.ActivateView("hangar")

        vss_mod.ViewStateSvc = ViewStateSvc
        sys.modules["eve.client.script.ui.services.viewStateSvc"] = vss_mod

        builtins.session = types.SimpleNamespace(stationid=None, structureid=1001896944936, shipid=1897518389)
        evomu_site._install_camera_dock_guard()

        # Case 1: Secondary view SkillPlanner closes after Board Ship while PlaceShip returns None -> no crash, fades from black, no None AllignCamera.
        cam1 = ModularHangarCamera()
        beh1 = ModularHangarShipBehaviour(ship_to_place=None, small_docks=[], repopulate_dock=False)
        view1 = HangarView(beh1, cam1)
        svc1 = ViewStateSvc(view1)
        svc1.CloseSecondaryView()

        self.assertIn(("_FadeFromBlackHandler", 0.5, False), events)
        self.assertIsNone(cam1.model)
        self.assertEqual(cam1.yaw, 0)
        self.assertEqual(cam1.pitch, 0)
        self.assertEqual(cam1.GetSizeDependantZoomConstants(), (385.0, 10.0, 120.0))
        cam1.currentShipSizeClass = 2
        self.assertEqual(cam1.GetSizeDependantZoomConstants(), (1450.0, 250.0, 500.0))
        self.assertIsNone(cam1._updateShipChangeLocations())
        # No AllignCamera(model=None) was scheduled (delay 0.3); only bounded ship-placement retry (delay 0.5) was scheduled.
        self.assertEqual(len(delayed_calls), 1)
        self.assertEqual(delayed_calls[0][0], 0.5)

        # Case 1b: When retry fires after ship asset becomes available, it recovers activeShipModel and caps retries at 2 if still unavailable.
        beh1.smallDocks.append("dock_retry")
        beh1.ship_to_place = None
        delayed_calls[0][1]()  # Attempt #1 still fails -> schedules attempt #2.
        self.assertEqual(len(delayed_calls), 2)
        beh1.ship_to_place = None
        delayed_calls[1][1]()  # Attempt #2 reaches retry cap -> no attempt #3.
        self.assertEqual(len(delayed_calls), 2)

        # Reset retry state via successful placement on fresh call.
        venture_model = types.SimpleNamespace(
            name="1897518389",
            generatedShapeEllipsoidRadius=(12.0, 8.0, 24.0),
            rotationCurve=types.SimpleNamespace(currentValue=(0.0, 0.0, 0.0, 1.0)),
        )
        delayed_calls.clear()
        events.clear()
        beh2 = ModularHangarShipBehaviour(ship_to_place=None, small_docks=["dock_ok"], repopulate_dock=False)
        cam2 = ModularHangarCamera()
        view2 = HangarView(beh2, cam2)
        view2.ReplaceExistingShipModel(hv_mod.REPLACE_UPDATE)
        self.assertEqual(len(delayed_calls), 1)
        beh2.ship_to_place = venture_model
        delayed_calls[0][1]()
        self.assertIs(view2.activeShipModel, venture_model)
        self.assertEqual(view2._evomu_ship_retry_count, 0)
        # AllignCamera was now scheduled cleanly for the non-None model and runs without error.
        self.assertEqual(delayed_calls[-1][0], 0.3)
        delayed_calls[-1][1]()
        self.assertIn("AllignCamera", events)

        # Case 2: Empty docks from deferred LoadView get rebuilt via SetAnchorPoint before PlaceShip on reopening hangar.
        delayed_calls.clear()
        events.clear()
        cam3 = ModularHangarCamera()
        beh3 = ModularHangarShipBehaviour(ship_to_place=venture_model, small_docks=[], repopulate_dock=True)
        view3 = HangarView(beh3, cam3)
        ViewStateSvc(view3).CloseSecondaryView()
        self.assertIs(view3.activeShipModel, venture_model)
        self.assertTrue(any(ev[0] == "SetAnchorPoint" for ev in events))
        self.assertIn(("_FadeFromBlackHandler", 0.5, False), events)

        # Case 3: GetCamera() -> None during _ReplaceExistingShipModelModular uses _NullHangarCamera cleanly and cleans override.
        events.clear()
        delayed_calls.clear()
        beh4 = ModularHangarShipBehaviour(ship_to_place=None, small_docks=[], repopulate_dock=False)
        view4 = HangarView(beh4, cam=None)
        view4._ReplaceExistingShipModelModular(hv_mod.REPLACE_UPDATE, 1897518389, 32880, False)
        self.assertIsNone(view4.GetCamera())
        self.assertIn(("_FadeFromBlackHandler", 0.5, False), events)
        # Deferred retry from view4 does nothing if view is cleared before timer fires.
        view4.ClearScene()
        events.clear()
        delayed_calls[0][1]()
        self.assertEqual(events, [])

    def test_1318_modular_capital_camera_and_negative_none_masking_checks(self):
        mhc_mod = types.ModuleType("eve.client.script.ui.camera.modularHangarCamera")

        class ModularHangarCamera(object):
            def __init__(self):
                self.model = None
                self.currentShipSizeClass = 1
                self._eyePosition = (0.0, 0.0, 0.0)
                self.atPosition = (0.0, 0.0, 0.0)

            def _CalculateFocalPoint(self, model):
                return (0.0, 0.0, 0.0)

            def SetEyePosition(self, pos):
                self._eyePosition = pos

            def GetSizeDependantZoomConstants(self):
                _ = max(self.model.generatedShapeEllipsoidRadius)
                return (385.0, 10.0, 120.0)

            def CalculateTargetPoint(self):
                _ = max(self.model.generatedShapeEllipsoidRadius)
                return (1.0, 2.0, 3.0)

            def _updateShipChangeLocations(self):
                _ = max(self.model.generatedShapeEllipsoidRadius)

            def AnimEnterHangar(self, model, startPos, endPos, duration=None):
                self.model = model
                self.SetEyePosition(self.CalculateTargetPoint())

        mhc_mod.ModularHangarCamera = ModularHangarCamera
        sys.modules["eve.client.script.ui.camera.modularHangarCamera"] = mhc_mod

        mhcc_mod = types.ModuleType("eve.client.script.ui.camera.modularHangarCapitalCamera")

        class ModularHangarCapitalCamera(object):
            def __init__(self):
                self.model = None
                self.orbitTarget = (3150.0, 2220.0, 16500.0)
                self.poiTarget = (0.0, -500.0, 7500.0)
                self._eyePosition = (0.0, 0.0, 0.0)
                self.lerpingIntoPosition = False
                self.orbitUpdateThread = "running"

            def SetEyePosition(self, pos):
                self._eyePosition = pos

            def _configureCameraStartLocations(self, typeID, wasOrbiting=False):
                if wasOrbiting:
                    _ = self.model.modelWorldPosition
                    return "orbiting"
                return ("default", typeID)

            def CalculateOrbitTarget(self):
                _ = self.model.GetBoundingSphereRadius()
                _ = self.model.modelWorldPosition
                return (100.0, 200.0, 300.0)

            def AnimEnterHangar(self, model, startPos, endPos, duration=None):
                self.CalculateOrbitTarget()
                self.SetEyePosition(self.orbitTarget)

            def Orbit(self, dx, dy):
                self.orbitTarget = self.CalculateOrbitTarget()
                self.poiTarget = self.model.modelWorldPosition
                return "orbited"

            def CapitalOrbitUpdateThread(self):
                try:
                    if not self.lerpingIntoPosition:
                        self.poiTarget = self.model.modelWorldPosition
                finally:
                    self.orbitUpdateThread = None

        mhcc_mod.ModularHangarCapitalCamera = ModularHangarCapitalCamera
        sys.modules["eve.client.script.ui.camera.modularHangarCapitalCamera"] = mhcc_mod

        # Late import hook installation via _EvomuMetaHook().
        evomu_site._EvomuMetaHook().find_module("late")
        self.assertIn("modularHangarCamera", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertIn("modularHangarCapitalCamera", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)

        cap_cam = ModularHangarCapitalCamera()
        self.assertIsNone(cap_cam.AnimEnterHangar(None, None, None))
        self.assertEqual(cap_cam._eyePosition, (3150.0, 2220.0, 16500.0))
        self.assertEqual(cap_cam.CalculateOrbitTarget(), (3150.0, 2220.0, 16500.0))
        self.assertIsNone(cap_cam.Orbit(1.0, 2.0))
        self.assertEqual(cap_cam._configureCameraStartLocations(19720, wasOrbiting=True), ("default", 19720))
        self.assertIsNone(cap_cam.CapitalOrbitUpdateThread())
        self.assertIsNone(cap_cam.orbitUpdateThread)

        # Negative assertions: unrelated AttributeError on non-None models MUST propagate unmasked.
        broken_std_model = object()
        std_cam = ModularHangarCamera()
        with self.assertRaises(AttributeError):
            std_cam.AnimEnterHangar(broken_std_model, None, None)
        with self.assertRaises(AttributeError):
            std_cam.CalculateTargetPoint()
        with self.assertRaises(AttributeError):
            std_cam.GetSizeDependantZoomConstants()
        with self.assertRaises(AttributeError):
            std_cam._updateShipChangeLocations()

        broken_cap_model = object()
        cap_cam.model = broken_cap_model
        cap_cam.orbitUpdateThread = "running"
        with self.assertRaises(AttributeError):
            cap_cam.AnimEnterHangar(broken_cap_model, None, None)
        with self.assertRaises(AttributeError):
            cap_cam.CalculateOrbitTarget()
        with self.assertRaises(AttributeError):
            cap_cam.Orbit(1.0, 2.0)
        with self.assertRaises(AttributeError):
            cap_cam._configureCameraStartLocations(19720, wasOrbiting=True)
        with self.assertRaises(AttributeError):
            cap_cam.CapitalOrbitUpdateThread()

    def _setup_fake_skill_plan_modules(self):
        import inspect
        import weakref

        class FakeCCPSignal(object):
            """Faithful replica of CCP packages/signals/signal.py Signal."""

            def __init__(self, signalName=None):
                self._signalName = signalName
                self._functions = weakref.WeakSet()
                self._methods = weakref.WeakKeyDictionary()

            def __len__(self):
                return len(self._functions) + sum(len(x) for x in self._methods.values())

            def __iter__(self):
                callables = []
                callables.extend(self._functions)
                for obj, funcs in self._methods.items():
                    callables.extend(func.__get__(obj) for func in funcs)
                return iter(callables)

            def __call__(self, *args, **kwargs):
                for call in self:
                    call(*args, **kwargs)

            def connect(self, slot):
                if inspect.ismethod(slot):
                    if slot.__self__ not in self._methods:
                        self._methods[slot.__self__] = weakref.WeakSet()
                    self._methods[slot.__self__].add(slot.__func__)
                elif inspect.isfunction(slot):
                    self._functions.add(slot)
                else:
                    raise TypeError("Signal connect requires a callable slot")

            def disconnect(self, slot):
                if inspect.ismethod(slot):
                    if slot.__self__ in self._methods:
                        self._methods[slot.__self__].discard(slot.__func__)
                else:
                    self._functions.discard(slot)

        sp_signals = types.ModuleType("skills.skillplan.skillPlanSignals")
        sp_signals.on_created = FakeCCPSignal("skillplansignals.on_created")
        sp_signals.on_saved = FakeCCPSignal("skillplansignals.on_saved")
        sp_signals.on_deleted = FakeCCPSignal("skillplansignals.on_deleted")

        sp_pkg = types.ModuleType("skills.skillplan")
        sp_pkg.skillPlanSignals = sp_signals

        sp_ui_signals = types.ModuleType("eve.client.script.ui.skillPlan.skillPlanUISignals")
        for sig_name in (
            "on_create_new_personal_button",
            "on_create_new_corp_button",
            "on_edit_button",
            "on_delete_button",
            "on_close_new_button",
            "on_selected",
            "on_close_button",
            "on_show_multi_training_msg",
            "on_show_alpha_training_msg",
        ):
            setattr(sp_ui_signals, sig_name, FakeCCPSignal(sig_name))

        ui_sp_pkg = types.ModuleType("eve.client.script.ui.skillPlan")
        ui_sp_pkg.skillPlanUISignals = sp_ui_signals

        events = []
        plans = {"53504348-055d-4a81-0000-0000000000c2"}
        close_raise = [None]

        class FakeSkillPlanService(object):
            def Delete(self, skill_plan_id):
                events.append(("svc.Delete", skill_plan_id))
                if skill_plan_id not in plans:
                    raise KeyError(f"Unable to delete skill plan with id: {skill_plan_id} as it's not found")
                plans.remove(skill_plan_id)
                sp_signals.on_deleted(skill_plan_id)
                return ("deleted", skill_plan_id)

        svc = FakeSkillPlanService()
        panel_mod = types.ModuleType("eve.client.script.ui.skillPlan.skillPlanPanel")
        panel_mod.skillPlanSignals = sp_signals
        panel_mod.skillPlanUISignals = sp_ui_signals

        class SkillPlanPanel(object):
            def __init__(self, label):
                self.label = label
                self.destroyed = False
                self.closed_calls = 0
                # Cyclic reference simulating V24 child button callback keeping closed panel alive.
                self.child_btn = types.SimpleNamespace(func=self.OnDeleteSkillPlanButton, parent=self)
                self.ApplyAttributes({})

            def ApplyAttributes(self, attributes):
                sp_signals.on_created.connect(self.OnSkillPlanCreated)
                sp_signals.on_saved.connect(self.OnSkillPlanSaved)
                sp_signals.on_deleted.connect(self.OnSkillPlanDeleted)
                sp_ui_signals.on_create_new_personal_button.connect(self.OnCreateNewPersonalSkillPlanButton)
                sp_ui_signals.on_create_new_corp_button.connect(self.OnCreateNewCorpSkillPlanButton)
                sp_ui_signals.on_edit_button.connect(self.OnEditSkillPlanButton)
                sp_ui_signals.on_delete_button.connect(self.OnDeleteSkillPlanButton)
                sp_ui_signals.on_close_new_button.connect(self.OnCloseNewSkillPlanButton)
                sp_ui_signals.on_selected.connect(self.OnSkillPlanSelected)
                sp_ui_signals.on_close_button.connect(self.OnSkillPlanCloseButton)
                sp_ui_signals.on_show_multi_training_msg.connect(self.OnShowMultiTrainingMessage)
                sp_ui_signals.on_show_alpha_training_msg.connect(self.OnShowAlphaTrainingMessage)

            def OnSkillPlanCreated(self, skillPlanID):
                events.append((self.label, "OnSkillPlanCreated", skillPlanID))
                return ("created", self.label, skillPlanID)

            def OnSkillPlanSaved(self, skillPlan):
                events.append((self.label, "OnSkillPlanSaved", skillPlan))
                return ("saved", self.label, skillPlan)

            def OnSkillPlanDeleted(self, skillPlanID):
                events.append((self.label, "OnSkillPlanDeleted", skillPlanID))
                return ("deleted_ui", self.label, skillPlanID)

            def OnCreateNewPersonalSkillPlanButton(self):
                events.append((self.label, "OnCreateNewPersonalSkillPlanButton"))
                return ("new_personal", self.label)

            def OnCreateNewCorpSkillPlanButton(self):
                events.append((self.label, "OnCreateNewCorpSkillPlanButton"))
                return ("new_corp", self.label)

            def OnEditSkillPlanButton(self, skillPlan):
                events.append((self.label, "OnEditSkillPlanButton", skillPlan))
                return ("edit", self.label, skillPlan)

            def OnDeleteSkillPlanButton(self, skillPlan):
                events.append((self.label, "OnDeleteSkillPlanButton", skillPlan))
                return svc.Delete(skillPlan)

            def OnCloseNewSkillPlanButton(self):
                events.append((self.label, "OnCloseNewSkillPlanButton"))
                return ("close_new", self.label)

            def OnSkillPlanSelected(self, skillPlan):
                if self.destroyed:
                    return None
                events.append((self.label, "OnSkillPlanSelected", skillPlan))
                return ("selected", self.label, skillPlan)

            def OnSkillPlanCloseButton(self):
                events.append((self.label, "OnSkillPlanCloseButton"))
                return ("close_btn", self.label)

            def OnShowMultiTrainingMessage(self):
                events.append((self.label, "OnShowMultiTrainingMessage"))
                return ("multi_msg", self.label)

            def OnShowAlphaTrainingMessage(self, message):
                events.append((self.label, "OnShowAlphaTrainingMessage", message))
                return ("alpha_msg", self.label, message)

            def Close(self):
                # Reproduces CCP V24 SkillPlanPanel.Close without disconnecting global signals.
                self.closed_calls += 1
                self.destroyed = True
                if close_raise[0] is not None:
                    raise close_raise[0]
                return ("closed", self.label)

        panel_mod.SkillPlanPanel = SkillPlanPanel

        sys.modules["skills.skillplan"] = sp_pkg
        sys.modules["skills.skillplan.skillPlanSignals"] = sp_signals
        sys.modules["eve.client.script.ui.skillPlan"] = ui_sp_pkg
        sys.modules["eve.client.script.ui.skillPlan.skillPlanUISignals"] = sp_ui_signals
        sys.modules["eve.client.script.ui.skillPlan.skillPlanPanel"] = panel_mod

        all_signals = [
            sp_signals.on_created,
            sp_signals.on_saved,
            sp_signals.on_deleted,
            sp_ui_signals.on_create_new_personal_button,
            sp_ui_signals.on_create_new_corp_button,
            sp_ui_signals.on_edit_button,
            sp_ui_signals.on_delete_button,
            sp_ui_signals.on_close_new_button,
            sp_ui_signals.on_selected,
            sp_ui_signals.on_close_button,
            sp_ui_signals.on_show_multi_training_msg,
            sp_ui_signals.on_show_alpha_training_msg,
        ]
        return types.SimpleNamespace(
            sp_signals=sp_signals,
            sp_ui_signals=sp_ui_signals,
            panel_mod=panel_mod,
            SkillPlanPanel=SkillPlanPanel,
            all_signals=all_signals,
            events=events,
            plans=plans,
            close_raise=close_raise,
        )

    def test_1332_skill_plan_panel_close_disconnects_all_12_signals_and_prevents_zombie_delete(self):
        fx = self._setup_fake_skill_plan_modules()
        self.assertTrue(evomu_site._install_skill_plan_panel_signal_guard())
        # Second install call is an idempotent no-op.
        self.assertFalse(evomu_site._install_skill_plan_panel_signal_guard())

        # Keep explicit strong references (in addition to cyclic refs) so GC never masks leaked slots.
        retained_closed_panels = []
        for idx in (1, 2):
            panel = fx.SkillPlanPanel(f"panel_{idx}")
            self.assertEqual(panel.Close(), ("closed", f"panel_{idx}"))
            self.assertTrue(panel.destroyed)
            # Idempotent second Close() must also remain safe.
            self.assertEqual(panel.Close(), ("closed", f"panel_{idx}"))
            retained_closed_panels.append(panel)

        active_panel = fx.SkillPlanPanel("panel_3")

        for sig in fx.all_signals:
            self.assertEqual(len(sig), 1, msg=sig._signalName)
            self.assertNotIn(retained_closed_panels[0], sig._methods)
            self.assertNotIn(retained_closed_panels[1], sig._methods)
            self.assertIn(active_panel, sig._methods)

        # Trigger the exact #1332 delete signal: must fire ONLY once on active panel_3.
        plan_id = "53504348-055d-4a81-0000-0000000000c2"
        fx.sp_ui_signals.on_delete_button(plan_id)
        self.assertEqual(
            fx.events,
            [
                ("panel_3", "OnDeleteSkillPlanButton", plan_id),
                ("svc.Delete", plan_id),
                ("panel_3", "OnSkillPlanDeleted", plan_id),
            ],
        )

        # Trigger all remaining 10 global signals: each fires ONLY on active_panel.
        fx.events.clear()
        fx.sp_signals.on_created("plan_new")
        fx.sp_signals.on_saved("plan_saved")
        fx.sp_ui_signals.on_create_new_personal_button()
        fx.sp_ui_signals.on_create_new_corp_button()
        fx.sp_ui_signals.on_edit_button("plan_edit")
        fx.sp_ui_signals.on_close_new_button()
        fx.sp_ui_signals.on_selected("plan_sel")
        fx.sp_ui_signals.on_close_button()
        fx.sp_ui_signals.on_show_multi_training_msg()
        fx.sp_ui_signals.on_show_alpha_training_msg("alpha_msg")
        self.assertEqual(
            fx.events,
            [
                ("panel_3", "OnSkillPlanCreated", "plan_new"),
                ("panel_3", "OnSkillPlanSaved", "plan_saved"),
                ("panel_3", "OnCreateNewPersonalSkillPlanButton"),
                ("panel_3", "OnCreateNewCorpSkillPlanButton"),
                ("panel_3", "OnEditSkillPlanButton", "plan_edit"),
                ("panel_3", "OnCloseNewSkillPlanButton"),
                ("panel_3", "OnSkillPlanSelected", "plan_sel"),
                ("panel_3", "OnSkillPlanCloseButton"),
                ("panel_3", "OnShowMultiTrainingMessage"),
                ("panel_3", "OnShowAlphaTrainingMessage", "alpha_msg"),
            ],
        )

        # Even direct calls on a destroyed panel's slots no-op cleanly.
        fx.events.clear()
        zombie = retained_closed_panels[0]
        self.assertIsNone(zombie.OnSkillPlanCreated("x"))
        self.assertIsNone(zombie.OnSkillPlanSaved("x"))
        self.assertIsNone(zombie.OnSkillPlanDeleted("x"))
        self.assertIsNone(zombie.OnCreateNewPersonalSkillPlanButton())
        self.assertIsNone(zombie.OnCreateNewCorpSkillPlanButton())
        self.assertIsNone(zombie.OnEditSkillPlanButton("x"))
        self.assertIsNone(zombie.OnDeleteSkillPlanButton("missing-plan-id"))
        self.assertIsNone(zombie.OnCloseNewSkillPlanButton())
        self.assertIsNone(zombie.OnSkillPlanSelected("x"))
        self.assertIsNone(zombie.OnSkillPlanCloseButton())
        self.assertIsNone(zombie.OnShowMultiTrainingMessage())
        self.assertIsNone(zombie.OnShowAlphaTrainingMessage("msg"))
        self.assertEqual(fx.events, [])

        # Negative assertions: active panel propagates real errors unmasked, and Close()
        # still disconnects all slots in `finally:` even when original Close() raises or signals are missing.
        with self.assertRaises(KeyError):
            active_panel.OnDeleteSkillPlanButton("missing-plan-id")

        fx.sp_signals.on_deleted = None
        orig_disconnect = fx.sp_ui_signals.on_edit_button.disconnect
        fx.sp_ui_signals.on_edit_button.disconnect = lambda _slot: (_ for _ in ()).throw(RuntimeError("boom"))
        fx.close_raise[0] = RuntimeError("close_failed")
        with self.assertRaises(RuntimeError) as ctx:
            active_panel.Close()
        self.assertEqual(str(ctx.exception), "close_failed")
        fx.sp_ui_signals.on_edit_button.disconnect = orig_disconnect
        for sig in fx.all_signals:
            if sig._signalName == "skillplansignals.on_deleted":
                continue
            self.assertEqual(len(sig), 0, msg=sig._signalName)
            self.assertNotIn(active_panel, sig._methods)

    def test_1332_late_import_meta_hook_view_state_and_preexisting_panel_migration(self):
        fx = self._setup_fake_skill_plan_modules()

        # Construct panels BEFORE the guard is installed (simulating first-import race):
        # pre_closed already leaked into Signal._methods; pre_live is still open.
        pre_closed = fx.SkillPlanPanel("pre_closed")
        pre_closed.Close()
        pre_live = fx.SkillPlanPanel("pre_live")
        for sig in fx.all_signals:
            self.assertEqual(len(sig), 2)
            self.assertIn(pre_closed, sig._methods)
            self.assertIn(pre_live, sig._methods)

        # 1. _EvomuMetaHook schedules _install_skill_plan_panel_signal_guard on lazy skillPlanPanel import.
        saved_panel_mod = sys.modules.pop("eve.client.script.ui.skillPlan.skillPlanPanel")
        scheduled = []
        sys.modules["uthread"] = types.SimpleNamespace(new=lambda fn: scheduled.append(fn))
        evomu_site._EvomuMetaHook().find_module("eve.client.script.ui.skillPlan.skillPlanPanel")
        self.assertEqual(scheduled, [evomu_site._install_skill_plan_panel_signal_guard])
        sys.modules["eve.client.script.ui.skillPlan.skillPlanPanel"] = saved_panel_mod
        self.assertTrue(scheduled[0]())
        self.assertIn("skillPlanPanel", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)
        for sig in fx.all_signals:
            self.assertEqual(len(sig), 1, msg=sig._signalName)
            self.assertNotIn(pre_closed, sig._methods)
            self.assertIn(pre_live, sig._methods)
        pre_live.Close()
        for sig in fx.all_signals:
            self.assertEqual(len(sig), 0, msg=sig._signalName)

        # Reset to verify ViewStateSvc._OpenView also installs the guard and migrates pre-existing panels.
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS.discard("skillPlanPanel")
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS.discard("viewStateSvc")
        sys.modules.pop("eve.client.script.ui.skillPlan.skillPlanPanel", None)

        vss_mod = types.ModuleType("eve.client.script.ui.services.viewStateSvc")

        class ViewStateSvc(object):
            def _OpenView(self, viewInfo, reopen=False, cameraID=None, **kwargs):
                return "opened"

            def ActivateView(self, name, **kwargs):
                return "activated"

        vss_mod.ViewStateSvc = ViewStateSvc
        sys.modules["eve.client.script.ui.services.viewStateSvc"] = vss_mod

        evomu_site._install_camera_dock_guard()
        self.assertNotIn("skillPlanPanel", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)

        fx2 = self._setup_fake_skill_plan_modules()
        pre_closed_2 = fx2.SkillPlanPanel("pre_closed_2")
        pre_closed_2.Close()
        pre_live_2 = fx2.SkillPlanPanel("pre_live_2")

        self.assertEqual(ViewStateSvc()._OpenView(types.SimpleNamespace(name="skillplan")), "opened")
        self.assertIn("skillPlanPanel", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)

        # Pre-existing destroyed panel was purged immediately; pre_live_2 was migrated cleanly.
        for sig in fx2.all_signals:
            self.assertEqual(len(sig), 1, msg=sig._signalName)
            self.assertNotIn(pre_closed_2, sig._methods)
            self.assertIn(pre_live_2, sig._methods)

        plan_id = "53504348-055d-4a81-0000-0000000000c2"
        fx2.sp_ui_signals.on_delete_button(plan_id)
        self.assertEqual(
            fx2.events,
            [
                ("pre_live_2", "OnDeleteSkillPlanButton", plan_id),
                ("svc.Delete", plan_id),
                ("pre_live_2", "OnSkillPlanDeleted", plan_id),
            ],
        )

        # Closing pre_live_2 disconnects its migrated wrapped slots cleanly as well.
        pre_live_2.Close()
        for sig in fx2.all_signals:
            self.assertEqual(len(sig), 0)
            self.assertNotIn(pre_live_2, sig._methods)


if __name__ == "__main__":
    unittest.main()
