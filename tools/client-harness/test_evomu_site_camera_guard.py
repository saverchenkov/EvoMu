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


class _FakeScene(object):
    def __init__(self):
        self.objects = []
        self.reflectionProbe = object()
        self.postprocess = object()


class _FakeCamera(object):
    def __init__(self):
        self.ships = []
        self.dock_anims = []
        self.switch_anims = []
        self.placed = []

    def SetShip(self, model, type_id):
        self.ships.append((model, type_id))

    def AnimEnterHangar(self, model, startPos=None, endPos=None, duration=None):
        self.dock_anims.append((model, startPos, endPos, duration))

    def AnimSwitchShips(self, model, startPos=None, endPos=None, duration=None):
        self.switch_anims.append((model, startPos, endPos, duration))

    def PlaceShip(self, end_pos):
        self.placed.append(end_pos)


class _FakeShipBehaviour(object):
    def __init__(self, on_load=None):
        self.on_load = on_load
        self.load_calls = []
        self.animate_calls = []
        self.place_calls = []
        self.cleaned = 0

    def CleanScene(self):
        self.cleaned += 1

    def LoadShipModel(self, item_id, type_id, **_kwargs):
        self.load_calls.append((item_id, type_id))
        if self.on_load is not None:
            self.on_load(item_id, type_id)
        return ("ship_model", item_id, type_id)

    def AnimateShipEntry(self, model, type_id, duration=0.0):
        self.animate_calls.append((model, type_id, duration))

    def PlaceShip(self, model, type_id):
        self.place_calls.append((model, type_id))

    def GetAnimStartPosition(self):
        return (0.0, 0.0, 0.0)

    def GetAnimEndPosition(self):
        return (1.0, 2.0, 3.0)

    def GetShipCenter(self, _ship, _type_id):
        return (0.0, 0.0, 0.0)

    def GetActiveDock(self):
        return object()

    def ShouldSwitchSkin(self, _skin_id):
        return True

    def IsInitialized(self):
        return True

    def UpdateActiveShipIDs(self, *_args):
        return None

    def ProcessSkinChange(self, *_args):
        return None


class TestHangarViewTeardownGuard(unittest.TestCase):
    def setUp(self):
        self._saved_modules = {
            k: sys.modules.get(k)
            for k in (
                "eve.client.script.ui.view.hangarView",
                "eve.client.script.ui.services.viewStateSvc",
                "stackless",
            )
        }
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS = set()
        self._setup_fake_hangar_view_module()

    def tearDown(self):
        for k, v in self._saved_modules.items():
            if v is None:
                sys.modules.pop(k, None)
            else:
                sys.modules[k] = v
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS = set()

    def _setup_fake_hangar_view_module(self):
        camera = _FakeCamera()
        self.camera = camera
        events = []
        self.events = events

        class _HangarView(object):
            def __init__(self, on_load=None, on_cosmetics=None, raise_attr_err=False):
                self.shipBehaviour = _FakeShipBehaviour(on_load=on_load)
                self.activeHangarScene = _FakeScene()
                self.activeHangarModel = object()
                self.activeShipModel = None
                self.on_cosmetics = on_cosmetics
                self.raise_attr_err = raise_attr_err

            def ClearScene(self):
                events.append("ClearScene")
                if self.shipBehaviour is not None:
                    self.shipBehaviour.CleanScene()
                    self.shipBehaviour = None
                if self.activeHangarScene is not None:
                    self.activeHangarScene.postprocess = None
                self.activeHangarScene = None
                self.activeHangarModel = None
                self.activeShipModel = None

            def RemoveAllShipsFromScene(self):
                events.append("RemoveAllShipsFromScene")
                del self.activeHangarScene.objects[:]

            def AddModelToScene(self, model):
                events.append("AddModelToScene")
                self.activeHangarScene.objects.append(model)
                self.activeShipModel = model
                if self.on_cosmetics is not None:
                    self.on_cosmetics(model)
                if self.raise_attr_err == "add":
                    raise AttributeError("unexpected_add_attr_error")

            def ReplaceExistingShipModel(self, eventType, fade=True, skipAnimation=False):
                events.append("ReplaceExistingShipModel")
                if self.raise_attr_err == "replace":
                    raise AttributeError("unexpected_replace_attr_error")
                if eventType == "modular":
                    self._ReplaceExistingShipModelModular(eventType, 1897147421, 649, skipAnimation)
                    return
                new_model = self.shipBehaviour.LoadShipModel(1897147421, 649)
                self.RemoveAllShipsFromScene()
                self.AddModelToScene(new_model)
                camera.SetShip(new_model, 649)
                self.shipBehaviour.AnimateShipEntry(new_model, 649, duration=7.0)
                if eventType == 2:
                    self.PlayDockSequence(7.0)
                elif eventType == 1:
                    self.PlaySwitchShipSequence(7.0)
                else:
                    self.PlayUpdateShipSequence(fade)
                self._SetUpLightingRig(self.activeShipModel)
                self.ConfigureVFXMultiEffect(eventType == 2)

            def _ReplaceExistingShipModelModular(self, _eventType, itemID, typeID, _skipAnimation):
                events.append("_ReplaceExistingShipModelModular")
                new_model = self.shipBehaviour.LoadShipModel(itemID, typeID)
                self.shipBehaviour.UpdateActiveShipIDs(itemID, typeID)
                self.activeHangarScene.objects.append(new_model)
                self.activeShipModel = new_model
                self.shipBehaviour.PlaceShip(new_model, typeID)

            def PlayDockSequence(self, duration):
                events.append("PlayDockSequence")
                if self.raise_attr_err == "dock_seq":
                    raise AttributeError("unexpected_dock_seq_attr_error")
                camera.AnimEnterHangar(
                    self.activeShipModel,
                    startPos=self.shipBehaviour.GetAnimStartPosition(),
                    endPos=self.shipBehaviour.GetAnimEndPosition(),
                    duration=duration,
                )

            def PlaySwitchShipSequence(self, duration):
                events.append("PlaySwitchShipSequence")
                camera.AnimSwitchShips(
                    self.activeShipModel,
                    startPos=self.shipBehaviour.GetAnimStartPosition(),
                    endPos=self.shipBehaviour.GetAnimEndPosition(),
                    duration=duration,
                )

            def PlayUpdateShipSequence(self, _fade):
                events.append("PlayUpdateShipSequence")
                camera.PlaceShip(self.shipBehaviour.GetAnimEndPosition())

            def _SetUpLightingRig(self, ship):
                events.append("_SetUpLightingRig")
                if self.activeHangarModel is None:
                    return None
                return self.shipBehaviour.GetShipCenter(ship, 649)

            def ConfigureVFXMultiEffect(self, _makeSceneBlack=False):
                events.append("ConfigureVFXMultiEffect")
                if self.activeHangarScene is None:
                    return None
                return self.shipBehaviour.GetActiveDock()

            def OnActiveShipSkinChange(self, itemID, skinID, updateSkin=True, reloadSubsystems=False):
                events.append("OnActiveShipSkinChange")
                if self.raise_attr_err == "skin":
                    raise AttributeError("unexpected_skin_attr_error")
                if not self.shipBehaviour.ShouldSwitchSkin(skinID):
                    return
                new_model = self.shipBehaviour.LoadShipModel(itemID, 649)
                self.activeHangarScene.objects.append(new_model)
                self.shipBehaviour.ProcessSkinChange(new_model)
                self.ConfigureVFXMultiEffect(False)

        self.HangarView = _HangarView
        hangar_mod = types.ModuleType("eve.client.script.ui.view.hangarView")
        hangar_mod.HangarView = _HangarView
        self.hangar_mod = hangar_mod
        sys.modules["eve.client.script.ui.view.hangarView"] = hangar_mod

    def _call_all_guarded_methods(self, view):
        view.ReplaceExistingShipModel(2)
        view._ReplaceExistingShipModelModular(2, 1897147421, 649, False)
        view.RemoveAllShipsFromScene()
        view.AddModelToScene(object())
        view.PlayDockSequence(7.0)
        view.PlaySwitchShipSequence(7.0)
        view.PlayUpdateShipSequence(True)
        view._SetUpLightingRig(object())
        view.ConfigureVFXMultiEffect(True)
        view.OnActiveShipSkinChange(1897147421, 42)

    def test_pre_call_teardown_none_behaviour_or_scene_with_and_without_prior_model(self):
        self.assertTrue(evomu_site._install_hangar_view_teardown_guard())

        # Case A: Torn down before any active ship model replacement ran.
        view_no_prior = self.HangarView()
        view_no_prior.ClearScene()
        self.events[:] = []
        self._call_all_guarded_methods(view_no_prior)
        self.assertEqual(self.events, [])

        # Case B: Prior active model replacement succeeded, then ClearScene tore down both fields.
        view_with_prior = self.HangarView()
        view_with_prior.ReplaceExistingShipModel(2)
        self.assertIsNotNone(view_with_prior.activeShipModel)
        view_with_prior.ClearScene()
        self.events[:] = []
        self._call_all_guarded_methods(view_with_prior)
        self.assertEqual(self.events, [])

        # Case C: Only shipBehaviour is None (activeHangarScene still set).
        view_no_behaviour = self.HangarView()
        view_no_behaviour.activeShipModel = ("prior_model", 1897147421, 649)
        view_no_behaviour.shipBehaviour = None
        self.events[:] = []
        self._call_all_guarded_methods(view_no_behaviour)
        self.assertEqual(self.events, [])

        # Case D: Only activeHangarScene is None (shipBehaviour still set).
        view_no_scene = self.HangarView()
        view_no_scene.activeHangarScene = None
        self.events[:] = []
        self._call_all_guarded_methods(view_no_scene)
        self.assertEqual(self.events, [])

    def test_mid_yield_teardown_during_load_ship_model_aborts_cleanly(self):
        self.assertTrue(evomu_site._install_hangar_view_teardown_guard())

        # First replace succeeds so a prior active ship model existed, then second replace
        # loses the scene mid-RPC inside LoadShipModel.
        view = self.HangarView()
        view.ReplaceExistingShipModel(2)
        prior_ships_len = len(self.camera.ships)
        prior_anim_len = len(view.shipBehaviour.animate_calls)

        def _clear_on_load(_item_id, _type_id):
            view.ClearScene()

        view.shipBehaviour.on_load = _clear_on_load
        self.events[:] = []
        view.ReplaceExistingShipModel(2)
        self.assertEqual(len(self.camera.ships), prior_ships_len)
        self.assertEqual(prior_anim_len, 1)
        self.assertNotIn("AddModelToScene", self.events)
        self.assertNotIn("PlayDockSequence", self.events)

    def test_mid_yield_teardown_during_add_model_to_scene_prevents_animate_ship_entry_crash(self):
        self.assertTrue(evomu_site._install_hangar_view_teardown_guard())

        for use_prior_replace in (False, True):
            self.events[:] = []
            self.camera.ships[:] = []
            view = self.HangarView()
            if use_prior_replace:
                view.ReplaceExistingShipModel(2)
                self.assertEqual(len(self.camera.ships), 1)

            def _clear_during_cosmetics(_model, target=view):
                target.ClearScene()

            view.on_cosmetics = _clear_during_cosmetics
            ships_before = len(self.camera.ships)
            view.ReplaceExistingShipModel(2)
            # Must abort cleanly after AddModelToScene yield before SetShip/AnimateShipEntry.
            self.assertEqual(len(self.camera.ships), ships_before)
            self.assertIsNone(view.shipBehaviour)
            self.assertIsNone(view.activeHangarScene)

    def test_rapid_clear_scene_and_redock_aborts_stale_replace_without_blocking_new_task(self):
        self.assertTrue(evomu_site._install_hangar_view_teardown_guard())

        fake_tasklets = {"curr": "t1"}

        class _FakeStackless(object):
            @staticmethod
            def getcurrent():
                return fake_tasklets["curr"]

        sys.modules["stackless"] = _FakeStackless()

        view = self.HangarView()

        def _teardown_and_redock(_item_id, _type_id):
            # Simulates undock ClearScene + rapid redock running on tasklet t2 while t1 yielded.
            view.ClearScene()
            view.shipBehaviour = _FakeShipBehaviour()
            view.activeHangarScene = _FakeScene()
            view.activeHangarModel = object()
            fake_tasklets["curr"] = "t2"
            view.ReplaceExistingShipModel(2)
            fake_tasklets["curr"] = "t1"

        view.shipBehaviour.on_load = _teardown_and_redock
        view.ReplaceExistingShipModel(2)
        # Only t2's new ship must be passed to camera/AnimateShipEntry; stale t1 must abort.
        self.assertEqual(len(self.camera.ships), 1)
        self.assertEqual(len(view.shipBehaviour.animate_calls), 1)
        self.assertEqual(view._evomu_replace_gens, {})

    def test_modular_replace_and_skin_change_mid_yield_teardown(self):
        self.assertTrue(evomu_site._install_hangar_view_teardown_guard())

        view_mod = self.HangarView()
        view_mod.shipBehaviour.on_load = lambda *_a: view_mod.ClearScene()
        view_mod.ReplaceExistingShipModel("modular")
        self.assertIsNone(view_mod.shipBehaviour)
        self.assertIsNone(view_mod.activeHangarScene)

        view_skin = self.HangarView()
        view_skin.shipBehaviour.on_load = lambda *_a: view_skin.ClearScene()
        view_skin.OnActiveShipSkinChange(1897147421, 77)
        self.assertIsNone(view_skin.shipBehaviour)
        self.assertIsNone(view_skin.activeHangarScene)

    def test_unrelated_attribute_error_when_hangar_valid_still_propagates(self):
        self.assertTrue(evomu_site._install_hangar_view_teardown_guard())

        for mode in ("replace", "add", "dock_seq", "skin"):
            view = self.HangarView(raise_attr_err=mode)
            with self.assertRaises(AttributeError):
                if mode == "skin":
                    view.OnActiveShipSkinChange(1897147421, 77)
                else:
                    view.ReplaceExistingShipModel(2)

    def test_open_view_and_valid_hangar_replace_flow(self):
        # Ensure ViewStateSvc._OpenView lazily installs the HangarView guard once hangarView loads.
        vss_mod = types.ModuleType("eve.client.script.ui.services.viewStateSvc")

        class _ViewType(object):
            Primary = 1
            Secondary = 2

        class _ViewInfo(object):
            def GetViewType(self):
                return _ViewType.Primary

        class _ViewStateSvc(object):
            def _OpenView(self, viewInfo, reopen=False, cameraID=None, **kwargs):
                return "opened"

        vss_mod.ViewType = _ViewType
        vss_mod.ViewStateSvc = _ViewStateSvc
        sys.modules["eve.client.script.ui.services.viewStateSvc"] = vss_mod

        # Install viewStateSvc hook BEFORE hangarView exists, then introduce hangarView.
        sys.modules.pop("eve.client.script.ui.view.hangarView", None)
        evomu_site._install_camera_dock_guard()
        self.assertNotIn("hangarView", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)

        sys.modules["eve.client.script.ui.view.hangarView"] = self.hangar_mod
        res = _ViewStateSvc()._OpenView(_ViewInfo())
        self.assertEqual(res, "opened")
        self.assertIn("hangarView", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)

        # Valid in-hangar flow executes all steps normally across dock, switch, update, modular, and skin paths.
        view = self.HangarView()
        view.ReplaceExistingShipModel(2)
        view.ReplaceExistingShipModel(1)
        view.ReplaceExistingShipModel(0)
        view.ReplaceExistingShipModel("modular")
        view.OnActiveShipSkinChange(1897147421, 99)
        self.assertEqual(len(self.camera.ships), 3)
        self.assertEqual(len(view.shipBehaviour.animate_calls), 3)
        self.assertEqual(len(view.shipBehaviour.place_calls), 1)
        self.assertEqual(len(self.camera.dock_anims), 1)
        self.assertEqual(len(self.camera.switch_anims), 1)
        self.assertEqual(len(self.camera.placed), 1)
        self.assertIn("_ReplaceExistingShipModelModular", self.events)
        self.assertIn("OnActiveShipSkinChange", self.events)
        self.assertIn("ConfigureVFXMultiEffect", self.events)

        # Also verify _EvomuMetaHook installs the guard once hangarView is imported later.
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS.discard("hangarView")
        evomu_site._EvomuMetaHook().find_module("anything")
        self.assertIn("hangarView", evomu_site._HOOKED_CAMERA_GUARD_TARGETS)

    def test_direct_none_teardown_without_clear_scene_still_aborts_safely(self):
        self.assertTrue(evomu_site._install_hangar_view_teardown_guard())

        def _null_behaviour_directly(_model):
            view.shipBehaviour = None
            view.activeHangarScene = None

        view = self.HangarView(on_cosmetics=_null_behaviour_directly)
        view.ReplaceExistingShipModel(2)
        self.assertEqual(len(self.camera.ships), 0)
        self.assertIsNone(view.shipBehaviour)


class TestHangarViewUndockMovemeGuard(unittest.TestCase):
    _SENTINEL = object()

    def setUp(self):
        self._saved_modules = {
            k: sys.modules.get(k)
            for k in (
                "eve.client.script.ui.view.hangarView",
                "eve.client.script.ui.view.transitions",
                "eve.client.script.ui.services.viewStateSvc",
            )
        }
        self._saved_session = getattr(builtins, "session", self._SENTINEL)
        self._saved_sm = getattr(builtins, "sm", self._SENTINEL)
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS = set()

    def tearDown(self):
        for k, v in self._saved_modules.items():
            if v is None:
                sys.modules.pop(k, None)
            else:
                sys.modules[k] = v
        if self._saved_session is self._SENTINEL:
            if hasattr(builtins, "session"):
                delattr(builtins, "session")
        else:
            builtins.session = self._saved_session
        if self._saved_sm is self._SENTINEL:
            if hasattr(builtins, "sm"):
                delattr(builtins, "sm")
        else:
            builtins.sm = self._saved_sm
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS = set()

    def _build_env(self, on_fade_sleep=None, on_inv_lookup=None, docked_error=None):
        events = []

        class _FakeSession(object):
            def __init__(self):
                self.charid = 90000001
                self.solarsystemid = 30004759
                self.stationid = None
                self.structureid = 1001896944936
                self.shipid = 1897362930

        sess = _FakeSession()
        builtins.session = sess

        class _FakeSceneManager(object):
            def FadeIn(self, duration=0.5):
                events.append(("sceneManager.FadeIn", duration))

            def FadeOut(self, duration=0.5, sleep=True):
                events.append(("sceneManager.FadeOut", duration, sleep))

        class _FakeServiceManager(object):
            def IsServiceRunning(self, name):
                return name == "sceneManager"

            def GetService(self, name):
                if name == "sceneManager":
                    return _FakeSceneManager()
                raise KeyError(name)

        builtins.sm = _FakeServiceManager()

        class _FakeInvCache(object):
            def GetInventory(self, _container_id):
                if on_inv_lookup is not None:
                    on_inv_lookup(sess)
                if sess.stationid is None and sess.structureid is None:
                    raise RuntimeError("CharacterNotAtStation")
                return [{"itemID": sess.shipid, "typeID": 587}]

        class _FadeLayer(object):
            def __init__(self):
                self.closed = False

            def Close(self):
                self.closed = True
                events.append("fadeLayer.Close")

        vss_mod = types.ModuleType("eve.client.script.ui.services.viewStateSvc")

        class _ViewType(object):
            Primary = 1
            Secondary = 2

        class _Transition(object):
            def StartTransition(self, fromView, toView):
                events.append("Transition.StartTransition")

            def EndTransition(self, fromView, toView):
                events.append("Transition.EndTransition")

        vss_mod.ViewType = _ViewType
        vss_mod.Transition = _Transition

        hangar_mod = types.ModuleType("eve.client.script.ui.view.hangarView")
        trans_mod = types.ModuleType("eve.client.script.ui.view.transitions")
        trans_mod.Transition = _Transition

        class _FakeFadeToBlackLiteTransition(_Transition):
            def StartTransition(self, fromView, toView):
                _Transition.StartTransition(self, fromView, toView)
                self.fadeLayer = _FadeLayer()
                events.append("FadeToBlackLiteTransition.StartTransition.sleep")
                if on_fade_sleep is not None:
                    on_fade_sleep(sess)

            def EndTransition(self, fromView, toView):
                events.append("FadeToBlackLiteTransition.EndTransition.sleep")
                self.fadeLayer.Close()
                del self.fadeLayer
                _Transition.EndTransition(self, fromView, toView)

        class _FakeHangarToHangarTransition(_FakeFadeToBlackLiteTransition):
            def __init__(self, hangar_view):
                self.hangarView = hangar_view

            def StartTransition(self, fromView, toView):
                _FakeFadeToBlackLiteTransition.StartTransition(self, fromView, toView)
                self.hangarView.ClearScene()
                self.hangarView.SetUpModels()

            def EndTransition(self, fromView, toView):
                self.hangarView.OnHangarToHangar()
                _FakeFadeToBlackLiteTransition.EndTransition(self, fromView, toView)

        trans_mod.FadeToBlackLiteTransition = _FakeFadeToBlackLiteTransition
        trans_mod.HangarToHangarTransition = _FakeHangarToHangarTransition

        class _FakeViewStateSvc(object):
            def __init__(self, hangar_view, transition):
                self.isOpeningView = None
                self.primaryInfo = None
                self.activeViewInfo = None
                self.hangar_view = hangar_view
                self.transition = transition

            def _OpenView(self, viewInfo, reopen=False, cameraID=None, **kwargs):
                events.append(("_OpenView", getattr(viewInfo, "name", None)))
                if getattr(viewInfo, "name", None) == "hangar":
                    self.hangar_view.LoadView()
                    self.hangar_view.ShowView()
                    self.hangar_view.LoadCamera(cameraID=cameraID, reopen=reopen)
                self.primaryInfo = viewInfo
                self.activeViewInfo = viewInfo

            def ActivateView(self, name, **kwargs):
                events.append(("ActivateView.begin", name))
                self.isOpeningView = name
                try:
                    info = types.SimpleNamespace(name=name, GetViewType=lambda: _ViewType.Primary)
                    if name == "hangar":
                        self.transition.StartTransition(self.hangar_view, self.hangar_view)
                        self._OpenView(info)
                        self.transition.EndTransition(self.hangar_view, self.hangar_view)
                    else:
                        self._OpenView(info)
                finally:
                    self.isOpeningView = None
                    events.append(("ActivateView.end", name))

        vss_mod.ViewStateSvc = _FakeViewStateSvc

        class _FakeHangarView(object):
            def __init__(self, view_svc):
                self.invCache = _FakeInvCache()
                self.view_svc = view_svc
                self.shipBehaviour = object()
                self.activeHangarScene = object()

            def ClearScene(self):
                events.append("HangarView.ClearScene")
                self.shipBehaviour = None
                self.activeHangarScene = None

            def GetShipItemFromHangar(self, shipID):
                events.append(("HangarView.GetShipItemFromHangar", shipID))
                items = self.invCache.GetInventory(10004)
                return items[0] if items else None

            def SetUpModels(self):
                events.append("HangarView.SetUpModels")
                if docked_error is not None:
                    raise docked_error
                item = self.GetShipItemFromHangar(sess.shipid)
                self.shipBehaviour = object()
                self.activeHangarScene = object()
                self.LoadAndSetupScene(item["typeID"], item["itemID"])

            def LoadAndSetupScene(self, shipTypeID, itemID=None, isANewHangar=False):
                events.append(("HangarView.LoadAndSetupScene", shipTypeID, itemID))

            def LoadView(self, change=None, **kwargs):
                events.append("HangarView.LoadView")
                self.SetUpModels()

            def ShowView(self):
                events.append("HangarView.ShowView")
                if not sess.stationid and not sess.structureid:
                    return

            def LoadCamera(self, cameraID=None, reopen=False):
                events.append("HangarView.LoadCamera")

            def ReloadView(self):
                events.append("HangarView.ReloadView")
                self.view_svc.ActivateView("hangar")

            def ProcessActiveShipChanged(self, shipID, oldShipID):
                events.append(("HangarView.ProcessActiveShipChanged", shipID, oldShipID))
                self.GetShipItemFromHangar(shipID)
                self.ReloadView()

            def OnHangarToHangar(self):
                events.append("HangarView.OnHangarToHangar")

        hangar_mod.HangarView = _FakeHangarView
        sys.modules["eve.client.script.ui.services.viewStateSvc"] = vss_mod
        sys.modules["eve.client.script.ui.view.hangarView"] = hangar_mod
        sys.modules["eve.client.script.ui.view.transitions"] = trans_mod

        evomu_site._install_camera_dock_guard()
        view_svc = _FakeViewStateSvc(None, None)
        hangar_view = _FakeHangarView(view_svc)
        transition = _FakeHangarToHangarTransition(hangar_view)
        view_svc.hangar_view = hangar_view
        view_svc.transition = transition
        return sess, view_svc, hangar_view, transition, events

    def test_structure_moveme_mid_hangar_to_hangar_fade_avoids_character_not_at_station(self):
        def _moveme_during_fade(sess):
            sess.structureid = None
            sess.solarsystemid = 30000142

        sess, view_svc, hangar_view, transition, events = self._build_env(on_fade_sleep=_moveme_during_fade)
        # While still docked in Upwell structure, active ship refresh triggers ReloadView().
        hangar_view.ProcessActiveShipChanged(sess.shipid, sess.shipid - 1)
        # Once ActivateView('hangar') clears isOpeningView, ChangePrimaryView('inflight') completes.
        self.assertIsNone(view_svc.isOpeningView)
        view_svc.ActivateView("inflight")

        self.assertIn("HangarView.ClearScene", events)
        self.assertIn("fadeLayer.Close", events)
        self.assertIn(("sceneManager.FadeOut", 0.0, False), events)
        self.assertIn("Transition.EndTransition", events)
        self.assertFalse(hasattr(transition, "fadeLayer"))
        self.assertNotIn("HangarView.SetUpModels", events)
        self.assertNotIn("HangarView.LoadView", events)
        self.assertNotIn("HangarView.LoadCamera", events)
        self.assertNotIn("HangarView.OnHangarToHangar", events)
        self.assertNotIn("FadeToBlackLiteTransition.EndTransition.sleep", events)
        self.assertEqual(view_svc.primaryInfo.name, "inflight")

    def test_hangar_view_methods_and_transition_noop_when_already_undocked(self):
        sess, view_svc, hangar_view, transition, events = self._build_env()
        sess.structureid = None
        sess.stationid = None
        sess.solarsystemid = 30000142

        self.assertIsNone(hangar_view.GetShipItemFromHangar(sess.shipid))
        self.assertIsNone(hangar_view.SetUpModels())
        self.assertIsNone(hangar_view.LoadAndSetupScene(587, sess.shipid))
        self.assertIsNone(hangar_view.LoadView())
        self.assertIsNone(hangar_view.LoadCamera())
        self.assertIsNone(hangar_view.ReloadView())
        self.assertIsNone(hangar_view.ProcessActiveShipChanged(sess.shipid, sess.shipid - 1))

        view_svc.ActivateView("hangar")
        self.assertIn("HangarView.ClearScene", events)
        self.assertNotIn("FadeToBlackLiteTransition.StartTransition.sleep", events)
        self.assertNotIn("HangarView.SetUpModels", events)
        self.assertNotIn("HangarView.OnHangarToHangar", events)
        self.assertFalse(hasattr(transition, "fadeLayer"))

    def test_mid_yield_character_not_at_station_absorbed_when_undocked_and_docked_works_normally(self):
        # Case A: session loses structureid inside invCache.GetInventory mid-SetUpModels.
        def _moveme_mid_inv(sess):
            sess.structureid = None
            sess.solarsystemid = 30000142

        _sess, _view_svc, hangar_view, _transition, events = self._build_env(on_inv_lookup=_moveme_mid_inv)
        self.assertIsNone(hangar_view.SetUpModels())
        self.assertIn("HangarView.SetUpModels", events)

        # Case B: while actually docked at an Upwell structure, HangarToHangar flow runs normally.
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS = set()
        sess_b, _view_svc_b, hangar_view_b, transition_b, events_b = self._build_env()
        hangar_view_b.ProcessActiveShipChanged(sess_b.shipid, sess_b.shipid - 1)
        self.assertIn("HangarView.SetUpModels", events_b)
        self.assertIn(("HangarView.LoadAndSetupScene", 587, sess_b.shipid), events_b)
        self.assertIn("HangarView.LoadView", events_b)
        self.assertIn("HangarView.LoadCamera", events_b)
        self.assertIn("HangarView.OnHangarToHangar", events_b)
        self.assertIn("FadeToBlackLiteTransition.EndTransition.sleep", events_b)
        self.assertFalse(hasattr(transition_b, "fadeLayer"))

        # Case C: genuine errors while still docked must still propagate cleanly.
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS = set()
        _sess_c, _view_svc_c, hangar_view_c, _tr_c, _ev_c = self._build_env(docked_error=RuntimeError("boom"))
        with self.assertRaises(RuntimeError) as ctx:
            hangar_view_c.SetUpModels()
        self.assertIn("boom", str(ctx.exception))


if __name__ == "__main__":
    unittest.main()
