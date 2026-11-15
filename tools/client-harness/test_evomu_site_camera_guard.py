# -*- coding: utf-8 -*-
"""Unit tests for evomu_site camera / secondary-view dock guard (#996)."""

import builtins
import math
import os
import sys
import types
import unittest

sys.modules.setdefault("__builtin__", builtins)

HARNESS_DIR = os.path.dirname(os.path.abspath(__file__))
if HARNESS_DIR not in sys.path:
    sys.path.insert(0, HARNESS_DIR)

import evomu_site  # noqa: E402


def _f32(x):
    """Simulate float32 overflow behavior matching V24.01 geo2 single-precision math."""
    val = float(x)
    if math.isnan(val) or math.isinf(val):
        return val
    if abs(val) > 3.4028234663852886e38:
        return math.copysign(float("inf"), val)
    return val


def _geo2_vec3_direction(from_pos, to_pos):
    dx = _f32(to_pos[0] - from_pos[0])
    dy = _f32(to_pos[1] - from_pos[1])
    dz = _f32(to_pos[2] - from_pos[2])
    sq = _f32(_f32(dx * dx) + _f32(dy * dy) + _f32(dz * dz))
    if math.isinf(sq) or sq == 0.0:
        return (float("nan"), float("nan"), float("nan"))
    length = math.sqrt(sq)
    return (dx / length, dy / length, dz / length)


def _geo2_vec2_length(v):
    sq = _f32(_f32(v[0] * v[0]) + _f32(v[1] * v[1]))
    if math.isinf(sq):
        return float("inf")
    return math.sqrt(sq)


def _geo2_vec3_normalize(v):
    sq = _f32(_f32(v[0] * v[0]) + _f32(v[1] * v[1]) + _f32(v[2] * v[2]))
    if math.isinf(sq) or sq == 0.0:
        return (float("nan"), float("nan"), float("nan"))
    length = math.sqrt(sq)
    return (v[0] / length, v[1] / length, v[2] / length)


class DestinyBallInvalid(Exception):
    pass


def _is_nan_or_inf_vec3(v):
    return any(math.isnan(float(c)) or math.isinf(float(c)) for c in v)


def _install_fake_v24_modules(events):
    """Register clean fake V24.01 camera/viewState modules mimicking exact CCP behavior."""
    evomu_site._HOOKED_CAMERA_GUARD_TARGETS.clear()

    class ViewType(object):
        Primary = 1
        Secondary = 2

    class _FakeViewInfo(object):
        def __init__(self, name, view_type):
            self.name = name
            self._view_type = view_type
            self.entryArguments = None

            view_name = name

            class _FakeLayer(object):
                name = view_name
                pickState = 0
                display = False

                def OpenView(self, **kw):
                    pass

            class _FakeView(object):
                layer = _FakeLayer()

                def LoadView(self, **kw):
                    pass

                def ShowView(self, **kw):
                    pass

                def LoadCamera(self, cameraID=None, reopen=False):
                    events.append(("LoadCamera", view_name))

            self.view = _FakeView()

        def GetViewType(self):
            return self._view_type

    class ViewStateSvc(object):
        def __init__(self, primary_info, active_info):
            self.primaryInfo = primary_info
            self.activeViewInfo = active_info

        def StartDependantServices(self, viewInfo):
            pass

        def _OpenView(self, viewInfo, reopen=False, cameraID=None, **kwargs):
            old_view = self.primaryInfo
            if viewInfo.GetViewType() == ViewType.Primary:
                self.primaryInfo = viewInfo
                builtins.sm.ScatterEvent("OnPrimaryViewChanged", old_view, self.primaryInfo)
            viewInfo.view.LoadView(lastView=old_view, **kwargs)
            viewInfo.view.LoadCamera(cameraID, reopen)

    class Camera(object):
        default_eyePosition = [0.0, 500.0, 1000.0]
        default_atPosition = [0.0, 0.0, 0.0]
        minZoom = 700000.0
        maxZoom = 13.877

        def __init__(self):
            self._eyePosition = list(self.default_eyePosition)
            self._atPosition = list(self.default_atPosition)
            self._eyeOffset = None
            self._atOffset = None
            self._eyeTransitOffset = None
            self._atTransitOffset = None
            self.isActive = True
            self.fov = 1.0

        @property
        def eyePosition(self):
            return self._eyePosition

        @property
        def atPosition(self):
            return self._atPosition

        def GetZoomToPoint(self):
            return self._atPosition

        def SetEyePosition(self, value):
            if _is_nan_or_inf_vec3(value):
                raise DestinyBallInvalid("Attempting to assign an invalid eyePosition value: %r" % (value,))
            if list(value) == list(self._atPosition):
                raise ValueError("same as atPosition")
            self._eyePosition = list(value)

        def SetAtPosition(self, value):
            if _is_nan_or_inf_vec3(value):
                raise DestinyBallInvalid("Attempting to assign an invalid atPosition value: %r" % (value,))
            if list(value) == list(self._eyePosition):
                raise ValueError("same as eyePosition")
            self._atPosition = list(value)

        def GetLookAtDirection(self):
            return _geo2_vec3_direction(self._eyePosition, self.GetZoomToPoint())

        def GetLookAtDirectionWithOffset(self):
            eye_pos = self._eyePosition
            at_pos = self._atPosition
            return _geo2_vec3_direction(eye_pos, at_pos)

        def OnDeactivated(self):
            self.isActive = False
            self._atOffset = None
            self._eyeOffset = None

    class _AttachedAnimator(object):
        def __init__(self, ball):
            self.ball = ball

    class _DetachedAnimator(object):
        ball = None

    class TacticalCamera(Camera):
        cameraID = "tactical"

        def __init__(self):
            Camera.__init__(self)
            self.positionAnimator = _DetachedAnimator()
            self.lastLookAtID = None

        def _SetLookAtBall(self, ball, duration=None):
            if ball:
                self.positionAnimator = _AttachedAnimator(ball)
            else:
                self.positionAnimator = _DetachedAnimator()

        def OnActivated(self, **kwargs):
            self.isActive = True
            last_cam = kwargs.get("lastCamera")
            if last_cam is not None:
                dist = max(self.maxZoom, min(70000.0, self.minZoom))
                self._ResetEyeAndAtPosition(self.eyePosition, self.atPosition, dist, True)

        def OnDeactivated(self):
            Camera.OnDeactivated(self)
            self._SetLookAtBall(None)

        def _GetNewDirection(self, direction):
            y = _geo2_vec2_length((direction[0], direction[2]))
            return _geo2_vec3_normalize((direction[0], y, direction[2]))

        def _ResetEyeAndAtPosition(self, eyePos0, atPos0, distance, animate=True, duration=None):
            diff = (eyePos0[0] - atPos0[0], eyePos0[1] - atPos0[1], eyePos0[2] - atPos0[2])
            direction = self._GetNewDirection(diff)
            eye_pos1 = [atPos0[0] + direction[0] * distance, atPos0[1] + direction[1] * distance, atPos0[2] + direction[2] * distance]
            if animate:
                dir_back = _geo2_vec3_direction(atPos0, eyePos0)
                at_shifted = [atPos0[0] + dir_back[0] * (distance / 5.0), atPos0[1] + dir_back[1] * (distance / 5.0), atPos0[2] + dir_back[2] * (distance / 5.0)]
                self.SetAtPosition(at_shifted)
                self.SetEyePosition(eye_pos1)
            else:
                self.SetAtPosition(atPos0)
                self.SetEyePosition(eye_pos1)

        def SetAtPosToFixedDistance(self):
            d = self.GetLookAtDirectionWithOffset()
            new_at = [self._eyePosition[0] + d[0] * -100.0, self._eyePosition[1] + d[1] * -100.0, self._eyePosition[2] + d[2] * -100.0]
            self.SetAtPosition(new_at)

        def Detach(self):
            self._SetLookAtBall(None)
            self.SetAtPosToFixedDistance()

        def OnBallRemoved(self, ball):
            if self.positionAnimator and self.positionAnimator.ball == ball:
                self.Detach()

        def SetAppropriateAtPositionForRotation(self, atPos):
            dx = atPos[0] - self.eyePosition[0]
            dy = atPos[1] - self.eyePosition[1]
            dz = atPos[2] - self.eyePosition[2]
            dist = min(1e7, math.sqrt(dx * dx + dy * dy + dz * dz))
            look_dir = self.GetLookAtDirection()
            new_at = [
                self.eyePosition[0] - look_dir[0] * dist,
                self.eyePosition[1] - look_dir[1] * dist,
                self.eyePosition[2] - look_dir[2] * dist,
            ]
            if new_at != self._eyePosition:
                self.SetAtPosition(new_at)

    vss_mod = types.ModuleType("eve.client.script.ui.services.viewStateSvc")
    vss_mod.ViewType = ViewType
    vss_mod.ViewStateSvc = ViewStateSvc
    sys.modules[vss_mod.__name__] = vss_mod

    base_cam_mod = types.ModuleType("eve.client.script.ui.camera.baseCamera")
    base_cam_mod.Camera = Camera
    base_cam_mod.DestinyBallInvalid = DestinyBallInvalid
    sys.modules[base_cam_mod.__name__] = base_cam_mod

    tac_mod = types.ModuleType("eve.client.script.ui.camera.tacticalCamera")
    tac_mod.TacticalCamera = TacticalCamera
    sys.modules[tac_mod.__name__] = tac_mod

    return _FakeViewInfo, ViewType, ViewStateSvc, Camera, TacticalCamera


class TestEvomuSiteCameraGuard(unittest.TestCase):
    def setUp(self):
        self.events = []
        self._orig_sm = getattr(builtins, "sm", None)
        (
            self.FakeViewInfo,
            self.ViewType,
            self.ViewStateSvc,
            self.Camera,
            self.TacticalCamera,
        ) = _install_fake_v24_modules(self.events)

    def tearDown(self):
        if self._orig_sm is None:
            if hasattr(builtins, "sm"):
                delattr(builtins, "sm")
        else:
            builtins.sm = self._orig_sm

    def _wire_sm(self, cam):
        events = self.events
        ship_ball = object()
        cam._SetLookAtBall(ship_ball)

        class _SceneManager(object):
            def GetActivePrimaryCamera(self):
                return cam

            def DeactivateFISCamera(self):
                events.append(("DeactivateFISCamera", cam.isActive))
                curr = self.GetActivePrimaryCamera()
                if curr and hasattr(curr, "OnDeactivated"):
                    curr.OnDeactivated()

        scene_mgr = _SceneManager()

        class _SM(object):
            def IsServiceRunning(self, name):
                return name == "sceneManager"

            def GetService(self, name):
                if name == "sceneManager":
                    return scene_mgr
                raise KeyError(name)

            def ScatterEvent(self, evt, *args):
                events.append((evt, args[0].name, args[1].name))
                if evt == "OnPrimaryViewChanged":
                    # Simulates michelle.OnPrimaryViewChanged -> RemoveBallpark -> Release -> RemoveBalls -> OnBallRemoved
                    cam.OnBallRemoved(ship_ball)

        builtins.sm = _SM()
        return ship_ball

    def test_secondary_view_dock_deactivates_fis_camera_before_remove_ballpark(self):
        cam = self.TacticalCamera()
        # Inject poisoned ~1e35 atPos from step_15121 before docking from skill_plan.
        cam._eyePosition = [-39999.97265625, 39999.86328125, 40000.20703125]
        cam._atPosition = [2.5724172177495162e35, 6.213082407888924e35, 6.097476634556235e35]
        self._wire_sm(cam)
        evomu_site._install_camera_dock_guard()

        inflight = self.FakeViewInfo("inflight", self.ViewType.Primary)
        skill_plan = self.FakeViewInfo("skill_plan", self.ViewType.Secondary)
        hangar = self.FakeViewInfo("hangar", self.ViewType.Primary)

        vss = self.ViewStateSvc(primary_info=inflight, active_info=skill_plan)
        vss._OpenView(hangar)

        self.assertFalse(cam.isActive)
        self.assertIsNone(cam.positionAnimator.ball)
        self.assertLess(max(abs(c) for c in cam._atPosition), 1e16)
        self.assertEqual(
            [e[0] for e in self.events],
            ["DeactivateFISCamera", "OnPrimaryViewChanged", "LoadCamera"],
        )

    def test_non_secondary_view_does_not_trigger_extra_deactivate(self):
        cam = self.TacticalCamera()
        self._wire_sm(cam)
        evomu_site._install_camera_dock_guard()

        inflight = self.FakeViewInfo("inflight", self.ViewType.Primary)
        hangar = self.FakeViewInfo("hangar", self.ViewType.Primary)

        vss = self.ViewStateSvc(primary_info=inflight, active_info=inflight)
        vss._OpenView(hangar)

        self.assertEqual(
            [e[0] for e in self.events],
            ["OnPrimaryViewChanged", "LoadCamera"],
        )

    def test_set_at_pos_to_fixed_distance_recovers_from_1e35_at_pos(self):
        cam = self.TacticalCamera()
        cam._eyePosition = [-39999.97265625, 39999.86328125, 40000.20703125]
        cam._atPosition = [2.5724172177495162e35, 6.213082407888924e35, 6.097476634556235e35]
        evomu_site._install_camera_dock_guard()

        cam.SetAtPosToFixedDistance()

        self.assertFalse(_is_nan_or_inf_vec3(cam._atPosition))
        self.assertLess(max(abs(c) for c in cam._atPosition), 1e16)

    def test_on_activated_and_reset_eye_and_at_recover_from_1e32_poison(self):
        cam = self.TacticalCamera()
        cam._eyePosition = [-138222.73637390137, 195751.22662353516, 138701.7545852661]
        cam._atPosition = [-3.0136833022548523e31, 6.151406842449182e32, -1.3739900122805895e31]
        evomu_site._install_camera_dock_guard()

        last_cam = self.TacticalCamera()
        last_cam._atPosition = [1e33, 2e33, 3e33]
        cam.OnActivated(lastCamera=last_cam)

        self.assertFalse(_is_nan_or_inf_vec3(cam._eyePosition))
        self.assertFalse(_is_nan_or_inf_vec3(cam._atPosition))
        self.assertLess(max(abs(c) for c in cam._atPosition), 1e16)

    def test_get_look_at_direction_and_rotation_recover_from_poison_without_nan(self):
        cam = self.TacticalCamera()
        cam._eyePosition = [-138222.73637390137, 195751.22662353516, 138701.7545852661]
        cam._atPosition = [-3.0136833022548523e31, 6.151406842449182e32, -1.3739900122805895e31]
        evomu_site._install_camera_dock_guard()

        d = cam.GetLookAtDirection()
        self.assertFalse(_is_nan_or_inf_vec3(d))
        cam.SetAppropriateAtPositionForRotation([1000.0, 2000.0, 3000.0])
        self.assertFalse(_is_nan_or_inf_vec3(cam._atPosition))

    def test_normal_coordinates_and_unrelated_errors_not_swallowed(self):
        cam = self.TacticalCamera()
        cam._eyePosition = [100.0, 200.0, 300.0]
        cam._atPosition = [0.0, 0.0, 0.0]
        evomu_site._install_camera_dock_guard()

        d = cam.GetLookAtDirectionWithOffset()
        expected_len = math.sqrt(100.0 ** 2 + 200.0 ** 2 + 300.0 ** 2)
        self.assertAlmostEqual(d[0], -100.0 / expected_len, places=6)
        self.assertAlmostEqual(d[1], -200.0 / expected_len, places=6)
        self.assertAlmostEqual(d[2], -300.0 / expected_len, places=6)

        with self.assertRaises(DestinyBallInvalid):
            cam.SetAtPosition([float("nan"), 0.0, 0.0])


class _FakeSignal(object):
    """Emulates V24.01 signals.signal.Signal bound-method storage (_methods[obj].add(func))."""

    def __init__(self):
        self._methods = {}
        self._functions = set()

    def connect(self, slot):
        if hasattr(slot, "__self__") and slot.__self__ is not None and hasattr(slot, "__func__"):
            self._methods.setdefault(slot.__self__, set()).add(slot.__func__)
        else:
            self._functions.add(slot)

    def __call__(self, *args, **kwargs):
        calls = []
        for obj, funcs in list(self._methods.items()):
            for func in list(funcs):
                calls.append(types.MethodType(func, obj))
        for fn in list(self._functions):
            calls.append(fn)
        for c in calls:
            c(*args, **kwargs)


class TestEvomuSiteLocationArrivalGuard(unittest.TestCase):
    """Tests for LocationArrivalIndicationService.on_warp_finished dock/ballpark teardown guard (#1051)."""

    def setUp(self):
        self._saved_modules = {
            k: sys.modules.get(k)
            for k in (
                "eve.client.script.remote.michelle",
                "eve.client.script.parklife.space_signals.warp_signals",
                "mining.client.location_arrival_indication_service",
                "eve.client.script.ui.shared.infoPanels.locationArrivalIndicationSvc",
            )
        }
        self._saved_sm = getattr(builtins, "sm", None)
        self._saved_session = getattr(builtins, "session", None)
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS = set()
        self._setup_fake_modules()

    def tearDown(self):
        for k, v in self._saved_modules.items():
            if v is None:
                sys.modules.pop(k, None)
            else:
                sys.modules[k] = v
        if self._saved_sm is None:
            if hasattr(builtins, "sm"):
                delattr(builtins, "sm")
        else:
            builtins.sm = self._saved_sm
        if self._saved_session is None:
            if hasattr(builtins, "session"):
                delattr(builtins, "session")
        else:
            builtins.session = self._saved_session
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS = set()

    def _setup_fake_modules(self):
        class _Session(object):
            def __init__(self):
                self.stationid = None
                self.structureid = None
                self.shipid = 100000100

        self.session = _Session()
        builtins.session = self.session

        class _Michelle(object):
            def __init__(self):
                self._Michelle__bp = None
                self._slim_items = {}
                self._balls_and_slims = []

            def GetBallpark(self, doWait=True):
                return self._Michelle__bp

            def GetSlimItem(self, item_id):
                if self._Michelle__bp is None:
                    return None
                return self._slim_items.get(item_id)

            def GetBallsAndSlimItems(self):
                # Exact V24.01 michelle.pyj behavior: returns None when __bp is None!
                if self._Michelle__bp is None:
                    return None
                return list(self._balls_and_slims)

        self.Michelle = _Michelle
        self.michelle = _Michelle()

        michelle_mod = types.ModuleType("eve.client.script.remote.michelle")
        michelle_mod.Michelle = _Michelle
        sys.modules["eve.client.script.remote.michelle"] = michelle_mod

        ws_mod = types.ModuleType("eve.client.script.parklife.space_signals.warp_signals")
        ws_mod.on_warp_activated = _FakeSignal()
        ws_mod.on_warp_arrived_at = _FakeSignal()
        ws_mod.on_warp_finished = _FakeSignal()
        self.warp_signals = ws_mod
        sys.modules["eve.client.script.parklife.space_signals.warp_signals"] = ws_mod

        michelle_ref = self.michelle
        indicated = []
        self.indicated = indicated

        class _LocationArrivalIndicationService(object):
            _instance = None

            @classmethod
            def initialize(cls):
                if not cls._instance:
                    cls._instance = cls()

            def __init__(self):
                self.tracked_slim_item = "prev_slim"
                ws_mod.on_warp_activated.connect(self.on_warp_activated)
                ws_mod.on_warp_arrived_at.connect(self.on_warp_arrived_at)
                ws_mod.on_warp_finished.connect(self.on_warp_finished)

            def on_warp_activated(self, item_id, bookmark_id):
                self.tracked_slim_item = None

            def on_warp_arrived_at(self, item_id):
                self._indicate_arrival(item_id)

            def on_warp_finished(self, item_id):
                self.check_indicate_arrival(item_id)

            def check_indicate_arrival(self, item_id):
                if self.tracked_slim_item is None or getattr(self.tracked_slim_item, "itemID", None) != item_id:
                    self._indicate_arrival(item_id)

            def _indicate_arrival(self, item_id):
                slim_item = michelle_ref.GetSlimItem(item_id)
                if slim_item is None:
                    slim_item = self.get_closest_location_slim_item()
                if slim_item is None:
                    self.tracked_slim_item = None
                    return
                self.tracked_slim_item = slim_item
                indicated.append(slim_item)

            def get_closest_location_slim_item(self):
                closest = None
                for _ball, slim_item in michelle_ref.GetBallsAndSlimItems():
                    closest = slim_item
                return closest

        self.LocationArrivalIndicationService = _LocationArrivalIndicationService
        mining_mod = types.ModuleType("mining.client.location_arrival_indication_service")
        mining_mod.LocationArrivalIndicationService = _LocationArrivalIndicationService
        mining_mod.GetMichelle = lambda: michelle_ref
        mining_mod.warp_signals = ws_mod
        sys.modules["mining.client.location_arrival_indication_service"] = mining_mod

        class _InfoPanelArrivalSvc(object):
            def __init__(self):
                self.tracked_slim_item = "prev"

            def GetBallpark(self):
                return michelle_ref.GetBallpark(False)

            def get_nearest_celestial_to_player(self):
                if self.GetBallpark() is None:
                    return None
                return ("celestial", 60003760)

            def on_warp_finished(self, item_id):
                for _item in self.get_nearest_celestial_to_player():
                    pass

        self.InfoPanelArrivalSvc = _InfoPanelArrivalSvc
        info_mod = types.ModuleType("eve.client.script.ui.shared.infoPanels.locationArrivalIndicationSvc")
        info_mod.LocationArrivalIndicationService = _InfoPanelArrivalSvc
        info_mod.get_nearest_celestial_to_player = lambda: (
            None if michelle_ref.GetBallpark(False) is None else ("celestial", 60003760)
        )
        sys.modules["eve.client.script.ui.shared.infoPanels.locationArrivalIndicationSvc"] = info_mod

        class _SM(object):
            def __init__(self, services):
                self.services = services

            def IsServiceRunning(self, name):
                return name in self.services

            def GetService(self, name):
                return self.services[name]

        self.info_panel_svc = _InfoPanelArrivalSvc()
        self.sm = _SM(
            {
                "michelle": self.michelle,
                "locationArrivalIndicationSvc": self.info_panel_svc,
            }
        )
        builtins.sm = self.sm

    def test_on_warp_finished_after_dock_or_release_ballpark_does_not_raise_type_error(self):
        # Simulate LocationArrivalIndicationService initialized BEFORE guard runs (signal already connected).
        self.LocationArrivalIndicationService.initialize()
        svc = self.LocationArrivalIndicationService._instance
        evomu_site._install_camera_dock_guard()

        # Case 1: Docked in station (stationid set, ballpark released -> None)
        self.session.stationid = 60003760
        self.michelle._Michelle__bp = None
        self.warp_signals.on_warp_finished(60003760)
        self.assertIsNone(svc.tracked_slim_item)
        self.assertEqual(self.indicated, [])

        # Case 2: Docked in structure (structureid set, ballpark released -> None)
        svc.tracked_slim_item = "prev"
        self.session.stationid = None
        self.session.structureid = 1040000000001
        self.warp_signals.on_warp_finished(1040000000001)
        self.assertIsNone(svc.tracked_slim_item)

        # Case 3: OnReleaseBallpark fired in-flight just before session.stationid update
        svc.tracked_slim_item = "prev"
        self.session.structureid = None
        self.michelle._Michelle__bp = None
        self.warp_signals.on_warp_finished(60003760)
        self.assertIsNone(svc.tracked_slim_item)
        self.assertIsNone(svc.get_closest_location_slim_item())
        self.assertEqual(self.michelle.GetBallsAndSlimItems(), [])

    def test_info_panel_arrival_svc_and_nearest_celestial_guard_when_none(self):
        evomu_site._install_camera_dock_guard()
        self.michelle._Michelle__bp = None
        self.session.stationid = 60003760

        self.assertIsNone(self.info_panel_svc.on_warp_finished(60003760))
        self.assertIsNone(self.info_panel_svc.tracked_slim_item)
        info_mod = sys.modules["eve.client.script.ui.shared.infoPanels.locationArrivalIndicationSvc"]
        self.assertIsNone(info_mod.get_nearest_celestial_to_player())

    def test_in_space_valid_ballpark_arrival_indication_succeeds(self):
        evomu_site._install_camera_dock_guard()
        self.LocationArrivalIndicationService.initialize()
        svc = self.LocationArrivalIndicationService._instance

        class _Slim(object):
            itemID = 60003760

        slim = _Slim()
        self.session.stationid = None
        self.session.structureid = None
        self.michelle._Michelle__bp = object()
        self.michelle._balls_and_slims = [(object(), slim)]

        self.warp_signals.on_warp_finished(60003760)
        self.assertIs(svc.tracked_slim_item, slim)
        self.assertEqual(self.indicated, [slim])


if __name__ == "__main__":
    unittest.main()
