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
            "modularHangarBehaviours",
            "wreck",
            "slotsContainer",
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

            def Prepare(self):
                self.SetRandomRotation()

        wreck_mod.Wreck = Wreck
        sys.modules["eve.client.script.environment.spaceObject.wreck"] = wreck_mod

        self.assertTrue(evomu_site._install_wreck_rotation_guard())

        no_model = Wreck(model=None)
        no_model.Prepare()
        self.assertEqual(no_model.rot, (1.0, 2.0, 3.0))

        # Unrelated AttributeError when model IS present must still propagate.
        bad_model = Wreck(model=object())
        with self.assertRaises(AttributeError):
            bad_model.SetRandomRotation()

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


if __name__ == "__main__":
    unittest.main()
