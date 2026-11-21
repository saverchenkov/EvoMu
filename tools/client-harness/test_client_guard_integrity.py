#!/usr/bin/env python3
"""Tests for client behaviour-guard hit recording, classification, ratchets, off switch, and telemetry (#1539)."""

import ast
import builtins
import contextlib
import importlib.machinery
import importlib.util
import json
import os
import re
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

HARNESS_DIR = Path(__file__).resolve().parent
if str(HARNESS_DIR) not in sys.path:
    sys.path.insert(0, str(HARNESS_DIR))

_orig_builtin = sys.modules.get("__builtin__")
sys.modules["__builtin__"] = builtins
try:
    import evomu_site  # noqa: E402
finally:
    if _orig_builtin is not None:
        sys.modules["__builtin__"] = _orig_builtin
    else:
        sys.modules.pop("__builtin__", None)

import guard_registry  # noqa: E402
import playtester  # noqa: E402


def _load_py2_harness_module(filename, stub_helpers=False):
    """Load a client-side Python 2.7 harness module under Python 3 with temporary stubs."""
    mod_path = HARNESS_DIR / filename
    orig_builtin = sys.modules.get("__builtin__")
    orig_helpers = sys.modules.get("evomu_inclient_helpers")
    sys.modules["__builtin__"] = builtins
    if stub_helpers and orig_helpers is None:
        sys.modules["evomu_inclient_helpers"] = types.ModuleType("evomu_inclient_helpers")
    try:
        spec = importlib.util.spec_from_file_location(
            f"_test_guard_{mod_path.stem}", str(mod_path)
        )
        mod = importlib.util.module_from_spec(spec)
        assert spec is not None and spec.loader is not None
        mod.__dict__.setdefault("unicode", str)
        mod.__dict__.setdefault("basestring", str)
        spec.loader.exec_module(mod)
        return mod
    finally:
        if orig_builtin is not None:
            sys.modules["__builtin__"] = orig_builtin
        else:
            sys.modules.pop("__builtin__", None)
        if stub_helpers and orig_helpers is None:
            sys.modules.pop("evomu_inclient_helpers", None)
        elif orig_helpers is not None:
            sys.modules["evomu_inclient_helpers"] = orig_helpers


def _load_run_journeys_module():
    path = HARNESS_DIR / "run_journeys"
    loader = importlib.machinery.SourceFileLoader("run_journeys_guard_test", str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


class _DummyUserError(Exception):
    def __init__(self, msg, info=None):
        super().__init__(msg, info or {})
        self.msg = msg
        self.dict = dict(info or {})


def _make_fake_session(
    *,
    stationid=None,
    structureid=None,
    solarsystemid=None,
    solarsystemid2=30000142,
    shipid=None,
    charid=90000001,
    mutating=False,
):
    return types.SimpleNamespace(
        stationid=stationid,
        structureid=structureid,
        solarsystemid=solarsystemid,
        solarsystemid2=solarsystemid2,
        shipid=shipid,
        charid=charid,
        corpid=1000009,
        warfactionid=None,
        mutating=mutating,
    )


def _make_fake_sm(services=None):
    svc_map = dict(services or {})
    return types.SimpleNamespace(
        services=svc_map,
        IsServiceRunning=lambda name: name in svc_map,
        GetService=lambda name: svc_map.get(name),
    )


@contextlib.contextmanager
def _isolated_evomu_site_env(*, env_overrides=None, session_obj=None, sm_obj=None, eve_obj=None):
    """Isolate evomu_site global state, sys.modules, builtins, and env for guard tests."""
    saved_modules = sys.modules.copy()
    saved_targets = set(evomu_site._HOOKED_CAMERA_GUARD_TARGETS)
    saved_log_targets = set(evomu_site._HOOKED_TARGETS)
    saved_hooked_lm = evomu_site._HOOKED_LOGMODULE
    saved_xmpp_prefs = evomu_site._HOOKED_XMPP_PREFS
    saved_xmpp_port = evomu_site._HOOKED_XMPP_PORT
    saved_cdn = evomu_site._HOOKED_FLATBUFFER_CDN
    saved_fsd = evomu_site._HOOKED_TYPELIST_PRELOAD
    saved_driver_scheduled = evomu_site._DRIVER_SCHEDULED
    saved_meta_installed = evomu_site._META_HOOK_INSTALLED
    saved_excepthook = sys.excepthook

    builtin_sentinel = object()
    saved_builtins = {
        attr: getattr(builtins, attr, builtin_sentinel)
        for attr in ("session", "sm", "eve", "prefs")
    }
    saved_env = os.environ.copy()

    try:
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS.clear()
        evomu_site._HOOKED_TARGETS.clear()
        evomu_site._HOOKED_LOGMODULE = False
        evomu_site._HOOKED_XMPP_PREFS = False
        evomu_site._HOOKED_XMPP_PORT = False
        evomu_site._HOOKED_FLATBUFFER_CDN = False
        evomu_site._HOOKED_TYPELIST_PRELOAD = False
        evomu_site._DRIVER_SCHEDULED = False
        evomu_site.reset_guard_hits()

        if env_overrides:
            for k, v in env_overrides.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = str(v)

        if session_obj is not None:
            builtins.session = session_obj
        if sm_obj is not None:
            builtins.sm = sm_obj
        if eve_obj is not None:
            builtins.eve = eve_obj

        yield
    finally:
        for mod_name in list(sys.modules.keys()):
            if mod_name not in saved_modules:
                sys.modules.pop(mod_name, None)
        sys.modules.update(saved_modules)

        evomu_site._HOOKED_CAMERA_GUARD_TARGETS.clear()
        evomu_site._HOOKED_CAMERA_GUARD_TARGETS.update(saved_targets)
        evomu_site._HOOKED_TARGETS.clear()
        evomu_site._HOOKED_TARGETS.update(saved_log_targets)
        evomu_site._HOOKED_LOGMODULE = saved_hooked_lm
        evomu_site._HOOKED_XMPP_PREFS = saved_xmpp_prefs
        evomu_site._HOOKED_XMPP_PORT = saved_xmpp_port
        evomu_site._HOOKED_FLATBUFFER_CDN = saved_cdn
        evomu_site._HOOKED_TYPELIST_PRELOAD = saved_fsd
        evomu_site._DRIVER_SCHEDULED = saved_driver_scheduled
        evomu_site._META_HOOK_INSTALLED = saved_meta_installed
        sys.excepthook = saved_excepthook
        evomu_site.reset_guard_hits()

        for attr, orig_val in saved_builtins.items():
            if orig_val is builtin_sentinel:
                if hasattr(builtins, attr):
                    delattr(builtins, attr)
            else:
                setattr(builtins, attr, orig_val)

        os.environ.clear()
        os.environ.update(saved_env)


def _register_fake_module(name, **attrs):
    mod = types.ModuleType(name)
    for k, v in attrs.items():
        setattr(mod, k, v)
    sys.modules[name] = mod
    return mod


class TestGuardRegistryAndRatchets(unittest.TestCase):
    """Verify guard registry structure, Markdown/evomu_site parity, Py2.7 compat, and ratchets."""

    def test_registry_invariants_and_client_only_governance(self):
        guards = guard_registry.BEHAVIOUR_GUARDS
        self.assertGreater(len(guards), 0)
        self.assertLessEqual(len(guards), guard_registry.MAX_BEHAVIOUR_GUARDS)
        self.assertEqual(len(guards), 15)

        seen_ids = set()
        seen_installers = set()
        approved_client_only = []
        for entry in guards:
            gid = entry["id"]
            inst = entry["installer"]
            cls_name = entry["classification"]
            self.assertNotIn(gid, seen_ids)
            self.assertNotIn(inst, seen_installers)
            seen_ids.add(gid)
            seen_installers.add(inst)
            self.assertIn(cls_name, guard_registry.VALID_CLASSIFICATIONS)
            self.assertTrue(entry["targets"], f"{gid} missing targets")

            if cls_name == guard_registry.CLASSIFICATION_SERVER_CAUSED:
                self.assertIsInstance(entry.get("open_issue"), int)
                self.assertGreater(entry["open_issue"], 0)
                self.assertFalse(entry["owner_approved"])
                self.assertFalse(guard_registry.is_approved_client_only(gid))
            else:
                case_txt = str(entry.get("client_only_case") or "").strip()
                self.assertTrue(case_txt, f"client-only guard {gid} missing written case")
                self.assertTrue(entry.get("owner_approved"), f"client-only guard {gid} lacks approval")
                self.assertTrue(guard_registry.is_approved_client_only(gid))
                approved_client_only.append(gid)

        # Today zero guards have client-only approval (#1539).
        self.assertEqual(approved_client_only, [])

    def test_registry_markdown_and_evomu_site_parity(self):
        md_text = (HARNESS_DIR / "CLIENT_GUARDS.md").read_text(encoding="utf-8")
        rows = []
        for line in md_text.splitlines():
            s = line.strip()
            if not s.startswith("| `"):
                continue
            cells = [c.strip().strip("`") for c in s.strip("|").split("|")]
            if len(cells) >= 8:
                rows.append(cells)

        self.assertEqual(len(rows), len(guard_registry.BEHAVIOUR_GUARDS))
        by_id = dict(guard_registry.BEHAVIOUR_GUARDS_BY_ID)
        for cells in rows:
            gid, installer, classification, open_issue_cell, named_cell, fix_cell, approved_cell, _ = cells
            self.assertIn(gid, by_id)
            spec = by_id[gid]
            self.assertEqual(installer, spec["installer"])
            self.assertEqual(classification, spec["classification"])
            self.assertEqual(open_issue_cell, f"#{spec['open_issue']}")
            expected_named = ", ".join(f"#{n}" for n in spec["named_issues"])
            self.assertEqual(named_cell, expected_named)
            self.assertEqual(fix_cell, spec["server_fix_status"])
            self.assertEqual(approved_cell, "yes" if spec["owner_approved"] else "no")

        site_src = (HARNESS_DIR / "evomu_site.py").read_text(encoding="utf-8")
        site_ast = ast.parse(site_src)
        defined_fns = {
            node.name for node in site_ast.body if isinstance(node, ast.FunctionDef)
        }
        registry_installers = {g["installer"] for g in guard_registry.BEHAVIOUR_GUARDS}
        out_of_scope = set(guard_registry.OUT_OF_SCOPE_ADAPTERS)

        for inst in registry_installers:
            self.assertIn(inst, defined_fns)

        # Every _install_*_guard in evomu_site.py must either be registered or explicitly out of scope.
        site_guard_installers = {
            name for name in defined_fns
            if name.startswith("_install_") and name.endswith("_guard")
        }
        self.assertEqual(
            site_guard_installers - out_of_scope - {"_install_cls_method_guard"},
            registry_installers,
        )

        # Every guard ID named in evomu_site._EvomuMetaHook must exist in guard_registry.
        referenced_ids = set(
            re.findall(r'_is_guard_enabled\(\s*["\']([a-z0-9_]+)["\']', site_src)
        ) | set(
            re.findall(r'_record_guard_hit\(\s*["\']([a-z0-9_]+)["\']', site_src)
        )
        self.assertEqual(referenced_ids, set(by_id.keys()))

    def test_benign_user_error_markers_monotonic_ratchet(self):
        markers = playtester.PlaytesterBugOracle.BENIGN_USER_ERROR_MARKERS
        self.assertLessEqual(
            len(markers),
            guard_registry.MAX_BENIGN_USER_ERROR_MARKERS,
            f"BENIGN_USER_ERROR_MARKERS grew above {guard_registry.MAX_BENIGN_USER_ERROR_MARKERS}: {len(markers)}",
        )
        self.assertEqual(len(markers), len(set(markers)))
        self.assertTrue(all(isinstance(m, str) and m.strip() for m in markers))

    def test_client_modules_python27_syntax_compatible(self):
        disallowed_types = (
            ast.JoinedStr,
            ast.AnnAssign,
            ast.Nonlocal,
            ast.YieldFrom,
            ast.AsyncFunctionDef,
            ast.AsyncFor,
            ast.AsyncWith,
            ast.Await,
        )
        # Note: Python 2.7 does support DictComp/SetComp (added in 2.7), only 3.0+ syntax like f-strings,
        # type annotations, kwonly args, yield from, nonlocal, and async/await is disallowed.
        for filename in (
            "guard_registry.py",
            "evomu_site.py",
            "evomu_playtester_hook.py",
            "evomu_inclient_driver.py",
        ):
            path = HARNESS_DIR / filename
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            for node in ast.walk(tree):
                self.assertNotIsInstance(
                    node,
                    disallowed_types,
                    f"Python 3-only syntax {type(node).__name__} found in {filename}:{getattr(node, 'lineno', '?')}",
                )
                if isinstance(node, ast.FunctionDef):
                    self.assertIsNone(
                        node.returns,
                        f"Return annotation found in {filename}:{node.lineno}",
                    )
                    self.assertFalse(
                        node.args.kwonlyargs,
                        f"Keyword-only args found in {filename}:{node.lineno}",
                    )
                if isinstance(node, ast.arg):
                    self.assertIsNone(
                        node.annotation,
                        f"Arg annotation found in {filename}",
                    )


class TestAllFifteenGuardsHitRecording(unittest.TestCase):
    """Exercise every behaviour guard wrapper's skip/catch/fallback/filter/sanitize paths."""

    def _assert_new_hit(self, prev_seq, expected_guard, expected_fn_substr, expected_reason_substr):
        hits = evomu_site.get_guard_hits(since_seq=prev_seq)
        matching = [
            h for h in hits
            if h.get("guard") == expected_guard
            and expected_fn_substr in str(h.get("function") or "")
            and expected_reason_substr in str(h.get("reason") or "")
        ]
        self.assertTrue(
            matching,
            f"Expected hit ({expected_guard}, {expected_fn_substr!r}, {expected_reason_substr!r}); got {hits!r}",
        )
        for h in hits:
            self.assertEqual(set(h.keys()), {"seq", "guard", "function", "reason", "time"})
            self.assertGreater(h["seq"], prev_seq)
        return evomu_site.get_guard_hit_seq()

    def test_1_camera_dock_guard_paths(self):
        cam = types.SimpleNamespace(isActive=True)
        deactivated = []
        scene_mgr = types.SimpleNamespace(
            GetActivePrimaryCamera=lambda: cam,
            DeactivateFISCamera=lambda: deactivated.append(True),
        )
        sess = _make_fake_session(stationid=None, structureid=None, solarsystemid=30000142)

        class FakeViewInfo:
            def __init__(self, name, vtype):
                self.name = name
                self._vtype = vtype

            def GetViewType(self):
                return self._vtype

        class FakeViewStateSvc:
            def __init__(self):
                self.primaryInfo = FakeViewInfo("inflight", 1)
                self.activeViewInfo = FakeViewInfo("map", 2)

            def _OpenView(self, viewInfo, reopen=False, cameraID=None, **kwargs):
                return "opened"

            def ActivateView(self, *args, **kwargs):
                return "activated"

        class FakeCamera:
            def GetZoomToPoint(self):
                return getattr(self, "_zoom_pt", (0.0, 0.0, 0.0))

            def GetLookAtDirectionWithOffset(self):
                return getattr(self, "_raw_dir", (0.0, 0.0, -1.0))

            def GetLookAtDirection(self):
                return getattr(self, "_raw_dir", (0.0, 0.0, -1.0))

        class FakeTacticalCamera:
            def OnActivated(self, **kwargs):
                return "on_act"

            def OnDeactivated(self):
                self._eyePosition = [float("nan"), 0.0, 0.0]
                return "on_deact"

            def _GetNewDirection(self, direction):
                return getattr(self, "_dir_res", (0.0, 0.0, -1.0))

            def _ResetEyeAndAtPosition(self, eyePos0, atPos0, distance, animate=True, duration=None):
                return (eyePos0, atPos0, animate)

            def SetAtPosToFixedDistance(self):
                return "fixed"

            def SetAppropriateAtPositionForRotation(self, atPos):
                return atPos

        class FakeMichelle:
            def __init__(self, bp=None):
                self._Michelle__bp = bp

            def GetBallsAndSlimItems(self):
                mode = getattr(self, "_mode", "ok")
                if mode == "typeerror":
                    raise TypeError("'NoneType' object is not iterable")
                if mode == "none":
                    return None
                return [1]

        class FakeArrivalSvc:
            def __init__(self):
                self.tracked_slim_item = "slim"

            def on_warp_finished(self):
                if getattr(self, "_raise_te", False):
                    raise TypeError("'NoneType' object is not subscriptable")
                return "arrived"

            def get_nearest_celestial_to_player(self):
                return "celestial"

        arrival_svc_inst = FakeArrivalSvc()
        arrival_svc_inst.on_warp_finished = lambda: FakeArrivalSvc.on_warp_finished(arrival_svc_inst)
        sm = _make_fake_sm({
            "sceneManager": scene_mgr,
            "michelle": FakeMichelle(bp="bp"),
            "locationArrivalIndicationSvc": arrival_svc_inst,
        })

        with _isolated_evomu_site_env(
            env_overrides={guard_registry.DISABLE_SERVER_GUARDS_ENV: "0"},
            session_obj=sess,
            sm_obj=sm,
        ):
            vss_mod = _register_fake_module(
                "eve.client.script.ui.services.viewStateSvc",
                ViewType=types.SimpleNamespace(Primary=1, Secondary=2),
                ViewStateSvc=FakeViewStateSvc,
            )
            _register_fake_module("eve.client.script.ui.camera.baseCamera", Camera=FakeCamera)
            _register_fake_module("eve.client.script.ui.camera.tacticalCamera", TacticalCamera=FakeTacticalCamera)
            _register_fake_module("eve.client.script.remote.michelle", Michelle=FakeMichelle)

            class FakeLocationArrivalIndicationService(FakeArrivalSvc):
                pass

            def mod_nearest():
                if getattr(sess, "_raise_mod_te", False):
                    raise TypeError("'NoneType' has no attribute")
                return "near"

            _register_fake_module(
                "mining.client.location_arrival_indication_service",
                LocationArrivalIndicationService=FakeLocationArrivalIndicationService,
                get_nearest_celestial_to_player=mod_nearest,
            )

            evomu_site._install_camera_dock_guard()

            seq = evomu_site.get_guard_hit_seq()
            vss = vss_mod.ViewStateSvc()
            vss._OpenView(FakeViewInfo("hangar", 1))
            seq = self._assert_new_hit(seq, "camera_dock_guard", "ViewStateSvc._OpenView", "deactivate_fis_camera_on_dock")
            self.assertEqual(deactivated, [True])

            base_cam = FakeCamera()
            base_cam._atPosition = [float("nan"), 0.0, 0.0]
            base_cam.GetLookAtDirectionWithOffset()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "Camera.GetLookAtDirectionWithOffset", "sanitized_camera_state")

            base_cam._eyePosition = [0.0, 500.0, 1000.0]
            base_cam._raw_dir = (float("inf"), 0.0, 0.0)
            base_cam.GetLookAtDirectionWithOffset()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "Camera.GetLookAtDirectionWithOffset", "non_finite_direction_result")

            base_cam._zoom_pt = (float("nan"), 0.0, 0.0)
            base_cam.GetLookAtDirection()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "Camera.GetLookAtDirection", "non_finite_camera_coords")
            base_cam._zoom_pt = (0.0, 0.0, 0.0)
            base_cam.GetLookAtDirection()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "Camera.GetLookAtDirection", "non_finite_direction_result")
            base_cam._raw_dir = (0.0, 0.0, -1.0)
            base_cam._atPosition = [float("nan"), 0.0, 0.0]
            base_cam.GetLookAtDirection()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "Camera.GetLookAtDirection", "sanitized_camera_state")

            tac = FakeTacticalCamera()
            tac._atPosition = [float("nan"), 0.0, 0.0]
            tac.OnActivated()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "TacticalCamera.OnActivated", "sanitized_camera_state")
            tac.OnDeactivated()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "TacticalCamera.OnDeactivated", "sanitized_camera_state")
            tac._GetNewDirection((float("nan"), 0.0, 0.0))
            seq = self._assert_new_hit(seq, "camera_dock_guard", "TacticalCamera._GetNewDirection", "non_finite_direction_arg")
            tac._dir_res = (float("nan"), 0.0, 0.0)
            tac._GetNewDirection((0.0, 0.0, -1.0))
            seq = self._assert_new_hit(seq, "camera_dock_guard", "TacticalCamera._GetNewDirection", "non_finite_direction_result")
            tac._ResetEyeAndAtPosition((0.0, 0.0, 0.0), (0.0, 0.0, 0.0), 100.0)
            seq = self._assert_new_hit(seq, "camera_dock_guard", "TacticalCamera._ResetEyeAndAtPosition", "sanitized_eye_at_pos")
            tac._eyePosition = [0.0, 0.0, 0.0]
            tac._atPosition = [0.0, 0.0, 0.0]
            tac.SetAtPosToFixedDistance()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "TacticalCamera.SetAtPosToFixedDistance", "sanitized_camera_state")
            tac.SetAppropriateAtPositionForRotation((float("inf"), 0.0, 0.0))
            seq = self._assert_new_hit(seq, "camera_dock_guard", "TacticalCamera.SetAppropriateAtPositionForRotation", "sanitized_at_pos")

            mich_no_bp = FakeMichelle(bp=None)
            self.assertEqual(mich_no_bp.GetBallsAndSlimItems(), [])
            seq = self._assert_new_hit(seq, "camera_dock_guard", "Michelle.GetBallsAndSlimItems", "ballpark_missing")
            mich_bp = FakeMichelle(bp="bp")
            mich_bp._mode = "typeerror"
            self.assertEqual(mich_bp.GetBallsAndSlimItems(), [])
            seq = self._assert_new_hit(seq, "camera_dock_guard", "Michelle.GetBallsAndSlimItems", "caught:TypeError:")
            mich_bp._mode = "none"
            self.assertEqual(mich_bp.GetBallsAndSlimItems(), [])
            seq = self._assert_new_hit(seq, "camera_dock_guard", "Michelle.GetBallsAndSlimItems", "none_result_coerced_to_empty")

            arr_cls = sys.modules["mining.client.location_arrival_indication_service"].LocationArrivalIndicationService
            arr = arr_cls()
            arr._raise_te = True
            arr.on_warp_finished()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "LocationArrivalIndicationService.on_warp_finished", "caught:TypeError:")
            sess._raise_mod_te = True
            sys.modules["mining.client.location_arrival_indication_service"].get_nearest_celestial_to_player()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "LocationArrival.get_nearest_celestial_to_player", "caught:TypeError:")

            arrival_svc_inst._raise_te = True
            arrival_svc_inst.on_warp_finished()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "FakeArrivalSvc.on_warp_finished", "caught:TypeError:")

            sess.stationid = 60003760
            arr.on_warp_finished()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "LocationArrivalIndicationService.on_warp_finished", "docked_or_ballpark_missing")
            sys.modules["mining.client.location_arrival_indication_service"].get_nearest_celestial_to_player()
            seq = self._assert_new_hit(seq, "camera_dock_guard", "LocationArrival.get_nearest_celestial_to_player", "docked_or_ballpark_missing")
            arrival_svc_inst.on_warp_finished()
            self._assert_new_hit(seq, "camera_dock_guard", "FakeArrivalSvc.on_warp_finished", "docked_or_ballpark_missing")

    def test_2_base_space_camera_guard_paths(self):
        class FakeBaseSpaceCamera:
            def ResetAnchorPos(self):
                return "reset"

            def UpdateAnchorOffset(self):
                mode = getattr(self, "_mode", "ok")
                if mode == "poison_and_raise":
                    self._anchorOffset = [float("nan"), 0.0, 0.0]
                    raise RuntimeError("poison offset")
                if mode == "post_poison":
                    self._anchorOffset = [1e35, 0.0, 0.0]
                return "updated"

            def GetTrackPosition(self, *args, **kwargs):
                return getattr(self, "_track_res", (1.0, 2.0, 3.0))

        with _isolated_evomu_site_env(env_overrides={guard_registry.DISABLE_SERVER_GUARDS_ENV: "0"}):
            _register_fake_module(
                "eve.client.script.ui.camera.baseSpaceCamera",
                BaseSpaceCamera=FakeBaseSpaceCamera,
                GetBallPosition=lambda ball: getattr(ball, "pos", (0.0, 0.0, 0.0)),
            )
            self.assertTrue(evomu_site._install_base_space_camera_guard())
            cam = FakeBaseSpaceCamera()
            seq = evomu_site.get_guard_hit_seq()

            cam._anchorOffset = [float("nan"), 0.0, 0.0]
            cam.ResetAnchorPos()
            seq = self._assert_new_hit(seq, "base_space_camera_guard", "BaseSpaceCamera.ResetAnchorPos", "cleared_non_finite_anchor_state")

            cam._anchorBall = types.SimpleNamespace(ballpark=None)
            cam.UpdateAnchorOffset()
            seq = self._assert_new_hit(seq, "base_space_camera_guard", "BaseSpaceCamera.UpdateAnchorOffset", "cleared_stale_anchor_ball")

            cam._anchorBall = None
            cam._mode = "poison_and_raise"
            cam.UpdateAnchorOffset()
            seq = self._assert_new_hit(seq, "base_space_camera_guard", "BaseSpaceCamera.UpdateAnchorOffset", "caught:poison offset")

            cam._mode = "post_poison"
            cam.UpdateAnchorOffset()
            seq = self._assert_new_hit(seq, "base_space_camera_guard", "BaseSpaceCamera.UpdateAnchorOffset", "sanitized_post_anchor_state")

            cam._mode = "ok"
            cam._track_res = (float("inf"), 0.0, 0.0)
            self.assertEqual(cam.GetTrackPosition(), (0.0, 0.0, 0.0))
            seq = self._assert_new_hit(seq, "base_space_camera_guard", "BaseSpaceCamera.GetTrackPosition", "non_finite_track_position")

            cam._track_res = (1.0, 2.0, 3.0)
            cam._anchorOffset = [float("nan"), 0.0, 0.0]
            self.assertEqual(cam.GetTrackPosition(), (1.0, 2.0, 3.0))
            self._assert_new_hit(seq, "base_space_camera_guard", "BaseSpaceCamera.GetTrackPosition", "sanitized_pre_anchor_state")

    def test_3_and_4_hangar_transition_and_view_teardown_guards(self):
        sess = _make_fake_session(stationid=60003760, shipid=101)

        class BaseTrans:
            def StartTransition(self, fromView, toView):
                return "base_start"

            def EndTransition(self, fromView, toView):
                return "base_end"

        class HangarToHangarTransition(BaseTrans):
            def StartTransition(self, fromView, toView):
                if getattr(self, "_undock_during", False):
                    sess.stationid = None
                    raise RuntimeError("mid-fade undock")
                return "start"

            def EndTransition(self, fromView, toView):
                return "end"

        class FakeHangarView:
            def __init__(self):
                self.shipBehaviour = types.SimpleNamespace(
                    playerShipItemID=101,
                    smallDocks=[],
                    bigDocks=[],
                    SetAnchorPoint=lambda m, s: self.shipBehaviour.smallDocks.append("dock0"),
                )
                self.activeHangarScene = "scene"
                self.activeHangarModel = "hmodel"
                self.activeShipModel = "smodel"
                self._fade_called = 0

            def _FadeFromBlackHandler(self, duration, sleep):
                self._fade_called += 1

            def ClearScene(self):
                if getattr(self, "_raise_none_clear", False):
                    raise AttributeError("'NoneType' object has no attribute 'ClearRefs'")
                return "cleared"

            def ReplaceExistingShipModel(self, *args, **kwargs):
                mode = getattr(self, "_replace_mode", "ok")
                if mode == "stale_mid_call":
                    self._evomu_hangar_gen = getattr(self, "_evomu_hangar_gen", 0) + 1
                    self._ReplaceExistingShipModelModular(*args, **kwargs)
                if mode == "stale_attr_err":
                    self._evomu_hangar_gen = getattr(self, "_evomu_hangar_gen", 0) + 1
                    raise AttributeError("scene gone")
                return "replaced"

            def _ReplaceExistingShipModelModular(self, *args, **kwargs):
                if getattr(self, "_clear_ship_model", False):
                    self.activeShipModel = None
                return "mod_replaced"

            def UnloadView(self):
                return "unloaded"

            def RemoveAllShipsFromScene(self):
                return "removed"

            def AddModelToScene(self):
                mode = getattr(self, "_add_mode", "ok")
                if mode == "stale_attr":
                    self._evomu_hangar_gen = getattr(self, "_evomu_hangar_gen", 0) + 1
                    raise AttributeError("missing rig")
                if mode == "stale_after":
                    self._evomu_hangar_gen = getattr(self, "_evomu_hangar_gen", 0) + 1
                return "added"

            def GetShipItemFromHangar(self):
                if getattr(self, "_undock_inside", False):
                    sess.stationid = None
                    raise RuntimeError("undocked inside call")
                return "ship_item"

            def OnActiveShipSkinChange(self, itemID):
                return f"skin:{itemID}"

            def ReloadView(self):
                return "reloaded"

            def PlayDockSequence(self):
                return "dock_seq"

            def PlaySwitchShipSequence(self):
                return "switch_seq"

            def PlayUpdateShipSequence(self):
                return "update_seq"

            def _SetUpLightingRig(self):
                return "lights"

            def ConfigureVFXMultiEffect(self):
                return "vfx"

            def SetUpModels(self):
                return "models"

            def LoadAndSetupScene(self):
                return "scene_loaded"

            def LoadView(self):
                return "view_loaded"

            def LoadCamera(self):
                return "cam_loaded"

            def ProcessActiveShipChanged(self):
                return "ship_changed"

        with _isolated_evomu_site_env(
            env_overrides={guard_registry.DISABLE_SERVER_GUARDS_ENV: "0"},
            session_obj=sess,
        ):
            _register_fake_module(
                "eve.client.script.ui.view.transitions",
                Transition=BaseTrans,
                HangarToHangarTransition=HangarToHangarTransition,
            )
            _register_fake_module(
                "eve.client.script.ui.view.hangarView",
                HangarView=FakeHangarView,
                REPLACE_UPDATE=1,
            )
            self.assertTrue(evomu_site._install_hangar_transition_guard())
            self.assertTrue(evomu_site._install_hangar_view_teardown_guard())

            seq = evomu_site.get_guard_hit_seq()
            tr = HangarToHangarTransition()
            tr.fadeLayer = types.SimpleNamespace(Close=lambda: None)
            tr._undock_during = True
            tr.StartTransition("h1", "h2")
            seq = self._assert_new_hit(seq, "hangar_transition_guard", "HangarToHangarTransition.StartTransition", "caught:mid-fade undock")
            tr.EndTransition("h1", "h2")
            seq = self._assert_new_hit(seq, "hangar_transition_guard", "HangarToHangarTransition.EndTransition", "aborted_or_undocked")
            tr.StartTransition("h1", "h2")
            seq = self._assert_new_hit(seq, "hangar_transition_guard", "HangarToHangarTransition.StartTransition", "session_undocked")

            sess.stationid = 60003760
            hv = FakeHangarView()
            hv._raise_none_clear = True
            hv.ClearScene()
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView.ClearScene", "caught:AttributeError:")

            hv._replace_mode = "stale_mid_call"
            hv.ReplaceExistingShipModel(1)
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView.ReplaceExistingShipModel", "caught:_HangarViewTornDown")

            hv._replace_mode = "stale_attr_err"
            hv.ReplaceExistingShipModel(1)
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView.ReplaceExistingShipModel", "caught:AttributeError:")

            hv.activeHangarScene = None
            hv.ReplaceExistingShipModel(1)
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView.ReplaceExistingShipModel", "hangar_view_torn_down")
            hv.PlayDockSequence()
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView.PlayDockSequence", "hangar_view_stale")

            hv.activeHangarScene = "scene"
            hv._add_mode = "stale_attr"
            hv.AddModelToScene()
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView.AddModelToScene", "caught:AttributeError:")
            hv._add_mode = "stale_after"
            hv.AddModelToScene()
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView.AddModelToScene", "hangar_view_stale_after_call")

            hv.shipBehaviour.smallDocks = []
            hv.shipBehaviour.bigDocks = []
            hv._ReplaceExistingShipModelModular(1)
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView._ReplaceExistingShipModelModular", "populated_docks_or_null_camera")

            hv.GetCamera = lambda: "cam"
            hv._clear_ship_model = True
            hv._ReplaceExistingShipModelModular(1)
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView._ReplaceExistingShipModelModular", "faded_in_or_scheduled_retry")

            hv.OnActiveShipSkinChange(999)
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView.OnActiveShipSkinChange", "stale_ship_skin_change")

            hv._undock_inside = True
            hv.GetShipItemFromHangar()
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView.GetShipItemFromHangar", "caught:undocked inside call")
            hv.LoadView()
            seq = self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView.LoadView", "session_undocked")
            hv.ReloadView()
            self._assert_new_hit(seq, "hangar_view_teardown_guard", "HangarView.ReloadView", "session_undocked")

    def test_5_inventory_transition_guard_paths(self):
        sess = _make_fake_session(stationid=None, structureid=None, shipid=101, mutating=False)
        dialogs = []
        eve_obj = types.SimpleNamespace(Message=lambda msg, d=None: dialogs.append((msg, d)))

        class FakeInventory:
            def __init__(self, win_id="", curr_inv=None, inv_cont=None):
                self.default_windowID = win_id
                self.currInvID = curr_inv
                self.invCont = inv_cont
                self.destroyed = False
                self.updateSelectedItemsPending = "pending"

            def _IsCurrentInventoryValid(self):
                if getattr(self, "_raise_valid", False):
                    raise KeyError("gone")
                return getattr(self, "_valid_ret", True)

            def OnSessionChanged(self, isRemote, s, change):
                return "changed"

            def _UpdateSelectedItems(self):
                if getattr(self, "_raise_upd", False):
                    raise _DummyUserError("OutOfRange", {"distance": 5000})
                return "updated"

            def _MoveItems(self):
                raise _DummyUserError("NotEnoughCargoSpace", {"need": 50})

        class FakeGauge:
            def __init__(self, inv_cont=None, destroyed=False):
                self.invCont = inv_cont
                self.destroyed = destroyed

            def SetSecondaryVolume(self, *args):
                if getattr(self, "_raise_err", False):
                    raise _DummyUserError("InventoryNotAccessible", {})
                return "vol"

            def UpdateLabel(self, *args):
                return "lbl"

        class FakeContainer:
            def __init__(self, moniker=None):
                self.moniker = moniker

            def List(self):
                raise AttributeError("'NoneType' object has no attribute 'List'")

        class StructureHangarController:
            itemID = 60003760

        with _isolated_evomu_site_env(
            env_overrides={guard_registry.DISABLE_SERVER_GUARDS_ENV: "0"},
            session_obj=sess,
            eve_obj=eve_obj,
        ):
            _register_fake_module("eve.client.script.ui.shared.inventory.invWindow", Inventory=FakeInventory)
            _register_fake_module("eve.client.script.ui.shared.container", InvContCapacityGauge=FakeGauge)
            _register_fake_module("eve.client.script.environment.invCache", invCacheContainer=FakeContainer)
            _register_fake_module("eveexceptions", UserError=_DummyUserError)

            self.assertTrue(evomu_site._install_inventory_transition_guard())
            seq = evomu_site.get_guard_hit_seq()

            w1 = FakeInventory(win_id="InventoryStation")
            self.assertFalse(w1._IsCurrentInventoryValid())
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "Inventory._IsCurrentInventoryValid", "undocked_window_id")

            w2 = FakeInventory(win_id="ShipCargo", curr_inv=("StructureItemHangar", 123))
            self.assertFalse(w2._IsCurrentInventoryValid())
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "Inventory._IsCurrentInventoryValid", "undocked_curr_inv_id")

            stale_inv_cont = types.SimpleNamespace(invController=StructureHangarController(), destroyed=False)
            w3 = FakeInventory(win_id="Other", curr_inv=("ActiveShip", 101), inv_cont=stale_inv_cont)
            self.assertFalse(w3._IsCurrentInventoryValid())
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "Inventory._IsCurrentInventoryValid", "stale_inventory_controller")

            w4 = FakeInventory(win_id="Other", curr_inv=("ShipCargo", 101))
            w4._raise_valid = True
            self.assertFalse(w4._IsCurrentInventoryValid())
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "Inventory._IsCurrentInventoryValid", "caught:")

            w_os = FakeInventory(curr_inv=None)
            w_os.OnSessionChanged(False, sess, {})
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "Inventory.OnSessionChanged", "cleared_pending_selection_update")

            w5 = FakeInventory()
            w5.destroyed = True
            w5._UpdateSelectedItems()
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "Inventory._UpdateSelectedItems", "destroyed_or_mutating")

            w6 = FakeInventory()
            w6._valid_ret = False
            w6._UpdateSelectedItems()
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "Inventory._UpdateSelectedItems", "inventory_invalid")

            w7 = FakeInventory(inv_cont=stale_inv_cont)
            w7._IsCurrentInventoryValid = lambda: True
            w7._UpdateSelectedItems()
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "Inventory._UpdateSelectedItems", "stale_inventory_controller")

            w8 = FakeInventory(curr_inv=("ShipCargo", 101))
            w8._raise_upd = True
            w8._IsCurrentInventoryValid = lambda: False
            orig_upd = FakeInventory._UpdateSelectedItems
            # Trigger caught path by becoming invalid/mutating during orig_upd
            sess_ref = sess
            def _mutating_upd(self):
                sess_ref.mutating = True
                raise _DummyUserError("OutOfRange", {})
            w8_cls = type("FakeMutInv", (FakeInventory,), {"_UpdateSelectedItems": lambda self: orig_upd.__closure__[0].cell_contents(self)})
            # Simpler: set sess.mutating inside valid custom instance where pre-checks passed:
            w_mut = FakeInventory(curr_inv=("ShipCargo", 101))
            w_mut._UpdateSelectedItems()  # clean pass
            w_mut._IsCurrentInventoryValid = lambda: True
            # Replace wrapped method's underlying call cleanly by subclassing? No, test directly via FakeInventory where _valid drops inside call:
            class FakeDropInv(FakeInventory):
                pass
            evomu_site._HOOKED_CAMERA_GUARD_TARGETS.discard("invWindow")
            FakeDropInv._UpdateSelectedItems = lambda self: (setattr(sess, "mutating", True), (_ for _ in ()).throw(_DummyUserError("OutOfRange", {})))
            sys.modules["eve.client.script.ui.shared.inventory.invWindow"].Inventory = FakeDropInv
            evomu_site._install_inventory_transition_guard()
            w_drop = FakeDropInv(curr_inv=("ShipCargo", 101))
            w_drop._UpdateSelectedItems()
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "Inventory._UpdateSelectedItems", "caught:")
            sess.mutating = False

            w_drop._MoveItems()
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "Inventory._MoveItems", "caught_user_error:")
            self.assertEqual(dialogs[0][0], "NotEnoughCargoSpace")

            g1 = FakeGauge(inv_cont=None, destroyed=False)
            g1.SetSecondaryVolume(1)
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "InvContCapacityGauge.SetSecondaryVolume", "stale_or_destroyed_gauge")

            g2 = FakeGauge(inv_cont=types.SimpleNamespace(destroyed=False, invController=None))
            class DropGauge(FakeGauge):
                def SetSecondaryVolume(self, *args):
                    self.destroyed = True
                    raise RuntimeError("mid-draw destroyed")
            evomu_site._HOOKED_CAMERA_GUARD_TARGETS.discard("invContainer")
            sys.modules["eve.client.script.ui.shared.container"].InvContCapacityGauge = DropGauge
            evomu_site._install_inventory_transition_guard()
            g_drop = DropGauge(inv_cont=types.SimpleNamespace(destroyed=False, invController=None))
            g_drop.SetSecondaryVolume(1)
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "InvContCapacityGauge.SetSecondaryVolume", "caught:mid-draw destroyed")

            c_none = FakeContainer(moniker=None)
            self.assertEqual(c_none.List(), [])
            seq = self._assert_new_hit(seq, "inventory_transition_guard", "invCacheContainer.List", "moniker_is_none")
            class DropContainer(FakeContainer):
                def List(self):
                    self.moniker = None
                    raise AttributeError("moniker cleared")
            evomu_site._HOOKED_CAMERA_GUARD_TARGETS.discard("invCacheContainer")
            sys.modules["eve.client.script.environment.invCache"].invCacheContainer = DropContainer
            evomu_site._install_inventory_transition_guard()
            DropContainer(moniker="m").List()
            self._assert_new_hit(seq, "inventory_transition_guard", "invCacheContainer.List", "caught:AttributeError:")

    def test_6_to_15_remaining_guards_all_paths(self):
        # Exercises guards #6 through #15 across every wrapper skip/catch/fallback/filter/sanitize path.
        sess = _make_fake_session(stationid=60003760, solarsystemid=None, shipid=2002)

        # --- Guard #6: fitting_active_ship_guard ---
        dogma_loc = types.SimpleNamespace(
            dogmaItems={2002: "active_ship"},
            currentShipID=2002,
            shipIDBeingDisembarked=1001,
            shipIDBeingEmbarked=2002,
            IsSwitchingShips=lambda: True,
        )
        ctrl = types.SimpleNamespace(
            IsSimulated=lambda: False,
            GetShipID=lambda: 1001,
            GetDogmaLocation=lambda: dogma_loc,
        )

        class FakeFittingSvc:
            def GetFittingDictForActiveShip(self):
                return {"ok": True}

        def evaluate_fit(fc):
            mode = getattr(fc, "_mode", "key")
            if mode == "index":
                raise IndexError("list index out of range on None")
            raise KeyError(1001)

        class FakeWarningIconCont:
            destroyed = False
            fittingController = ctrl

            def _WarningIconCont__UpdateIcon_thread(self):
                mode = getattr(self, "_mode", "key")
                if mode == "index":
                    raise IndexError("None sequence index")
                raise KeyError(1001)

        class FakeStatsPanel:
            destroyed = False
            controller = ctrl

            def UpdateStats(self):
                raise KeyError(1001)

            def UpdateDroneStats(self):
                raise KeyError(1001)

        class FakeDronePanel:
            destroyed = False
            controller = ctrl

            def UpdateDroneStats(self):
                raise KeyError(1001)

        # --- Guards #7, #8, #9: hangar behaviour guards ---
        class FakeBaseHangarShipBehaviour:
            def SetShipDamage(self, itemID, model):
                return True

        class FakeModularCapitalHangarShipBehaviour:
            playerShipItemID = None

            def ProcessSkinChange(self, newModel):
                return True

        class FakeModularHangarShipBehaviour:
            playerShipItemID = None
            smallDocks = None
            bigDocks = None

            def GetCamera(self):
                return getattr(self, "_cam", None)

            def CleanScene(self):
                if getattr(self, "_raise_clear_refs", False):
                    raise AttributeError("'NoneType' object has no attribute 'ClearRefs'")
                return "cleaned"

            def ProcessSkinChange(self, newModel):
                return True

        # --- Guard #10: wreck_rotation_guard ---
        class FakeWreck:
            model = None

            def Prepare(self):
                raise AttributeError("'NoneType' object has no attribute 'rotation'")

            def SetRandomRotation(self):
                raise AttributeError("NoneType")

            def SetDungeonRotation(self):
                raise AttributeError("NoneType")

            def SetBallRotation(self):
                raise AttributeError("NoneType")

        class FakeSpaceMgr:
            def LoadObject(self, *args):
                return "loaded"

        # --- Guard #11: slots_container_sync_guard ---
        class FakeSlotsContainer:
            __notifyevents__ = ["OnSessionChanged"]
            destroyed = False
            modulesByID = {}
            controller = types.SimpleNamespace(
                GetModules=lambda: [types.SimpleNamespace(itemID=7001)]
            )

            def InitSlotsDelayed(self):
                self._refreshed = getattr(self, "_refreshed", 0) + 1

            def ProcessShipEffect(self, godmaStm, effectState):
                return "pse"

            def OnInvChange(self, item, change):
                return "inv"

        # --- Guard #12: sensor_suite_gfx_guard ---
        class FakeGfxHandler:
            gfxSensorSwipe = None
            gfxSwipeThread = "t"
            sceneManager = None

            def StartGfxSwipe(self):
                raise RuntimeError("Couldn't get object")

            def _GfxSwipeThread(self):
                raise RuntimeError("Couldn't get object")

        # --- Guard #13: modular_hangar_camera_guard ---
        class FakeModularHangarCamera:
            model = None
            currentShipSizeClass = 2

            def AnimEnterHangar(self, model=None):
                return "entered"

            def CalculateTargetPoint(self):
                return "orig"

            def GetSizeDependantZoomConstants(self):
                return "orig"

            def _updateShipChangeLocations(self):
                return "orig"

        class FakeModularHangarCapitalCamera:
            model = None
            lerpingIntoPosition = False
            orbitUpdateThread = "t"

            def AnimEnterHangar(self, model=None):
                return "entered"

            def CalculateOrbitTarget(self):
                return "orig"

            def Orbit(self):
                return "orig"

            def _configureCameraStartLocations(self, typeID, wasOrbiting=False):
                return (typeID, wasOrbiting)

            def CapitalOrbitUpdateThread(self):
                raise AttributeError("model is None")

        # --- Guard #14: skill_plan_panel_signal_guard ---
        fake_sig = types.SimpleNamespace(_methods={})

        class FakeSkillPlanPanel:
            destroyed = False

            def OnSkillPlanCreated(self):
                return "ok"

            def OnCreateNewPersonalSkillPlanButton(self):
                return "ok"

            def Close(self):
                return "closed"

        fake_sig._methods[FakeSkillPlanPanel] = {FakeSkillPlanPanel.OnSkillPlanCreated}

        # --- Guard #15: probe_tracker_guard ---
        class FakeProbeTracker:
            probeData = {5001: {}}

            def OnProbesIdle(self, probes):
                for p in probes:
                    _ = self.probeData[p["probeID"]]
                return len(probes)

            def SetProbeDestination(self, probeID, pos):
                return self.probeData[probeID]

            def UpdateProbeState(self, probeID, state):
                return self.probeData[probeID]

        with _isolated_evomu_site_env(
            env_overrides={guard_registry.DISABLE_SERVER_GUARDS_ENV: "0"},
            session_obj=sess,
        ):
            _register_fake_module(
                "eve.client.script.environment.fittingSvc",
                FittingSvc=FakeFittingSvc,
                eveCfg=types.SimpleNamespace(GetActiveShip=lambda: None),
            )
            _register_fake_module("shipfitting.fittingWarnings", EvaluateFit=evaluate_fit)
            _register_fake_module(
                "eve.client.script.ui.shared.fittingScreen.warningIconCont",
                WarningIconCont=FakeWarningIconCont,
            )
            _register_fake_module(
                "eve.client.script.ui.shared.fittingScreen.statsPanel",
                StatsPanel=FakeStatsPanel,
            )
            _register_fake_module(
                "eve.client.script.ui.shared.fittingScreen.fittingPanels.dronePanel",
                DronePanel=FakeDronePanel,
            )
            _register_fake_module(
                "eve.client.script.ui.view.hangarBehaviours.baseHangarBehaviours",
                BaseHangarShipBehaviour=FakeBaseHangarShipBehaviour,
            )
            _register_fake_module(
                "eve.client.script.ui.view.hangarBehaviours.modularHangarCapitalBehaviours",
                ModularCapitalHangarShipBehaviour=FakeModularCapitalHangarShipBehaviour,
            )
            _register_fake_module(
                "eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours",
                ModularHangarShipBehaviour=FakeModularHangarShipBehaviour,
            )
            _register_fake_module("eve.client.script.environment.spaceObject.wreck", Wreck=FakeWreck)
            _register_fake_module("eve.client.script.parklife.spaceMgr", SpaceMgr=FakeSpaceMgr)
            _register_fake_module(
                "eve.client.script.ui.inflight.shipHud.slotsContainer",
                SlotsContainer=FakeSlotsContainer,
                IsShipFittingFlag=lambda f: f == 27,
            )
            _register_fake_module("sensorsuite.overlay.gfxhandler", GfxHandler=FakeGfxHandler)
            _register_fake_module(
                "eve.client.script.ui.camera.modularHangarCamera",
                ModularHangarCamera=FakeModularHangarCamera,
            )
            _register_fake_module(
                "eve.client.script.ui.camera.modularHangarCapitalCamera",
                ModularHangarCapitalCamera=FakeModularHangarCapitalCamera,
            )
            _register_fake_module(
                "eve.client.script.ui.skillPlan.skillPlanPanel",
                SkillPlanPanel=FakeSkillPlanPanel,
                skillPlanSignals=types.SimpleNamespace(on_created=fake_sig),
                skillPlanUISignals=types.SimpleNamespace(on_create_new_personal_button=fake_sig),
            )
            _register_fake_module("probescanning.probeTracker", ProbeTracker=FakeProbeTracker)

            evomu_site._install_live_client_sync_guards()
            seq = evomu_site.get_guard_hit_seq()

            # #6 fitting_active_ship_guard
            with self.assertRaises(RuntimeError):
                FakeFittingSvc().GetFittingDictForActiveShip()
            seq = self._assert_new_hit(seq, "fitting_active_ship_guard", "FittingSvc.GetFittingDictForActiveShip", "no_active_ship_raised")

            fw_mod = sys.modules["shipfitting.fittingWarnings"]
            fw_mod.EvaluateFit(ctrl)
            seq = self._assert_new_hit(seq, "fitting_active_ship_guard", "FittingWarnings.EvaluateFit", "caught:KeyError:")
            sess.shipid = None
            ctrl._mode = "index"
            fw_mod.EvaluateFit(ctrl)
            seq = self._assert_new_hit(seq, "fitting_active_ship_guard", "FittingWarnings.EvaluateFit", "caught:IndexError:")

            wic = FakeWarningIconCont()
            wic._mode = "index"
            wic._WarningIconCont__UpdateIcon_thread()
            seq = self._assert_new_hit(seq, "fitting_active_ship_guard", "WarningIconCont.__UpdateIcon_thread", "caught:IndexError:")
            sess.shipid = 2002
            wic._mode = "key"
            wic._WarningIconCont__UpdateIcon_thread()
            seq = self._assert_new_hit(seq, "fitting_active_ship_guard", "WarningIconCont.__UpdateIcon_thread", "caught:KeyError:")
            wic.destroyed = True
            wic._WarningIconCont__UpdateIcon_thread()
            seq = self._assert_new_hit(seq, "fitting_active_ship_guard", "WarningIconCont.__UpdateIcon_thread", "panel_destroyed")

            sp = FakeStatsPanel()
            sp.UpdateStats()
            seq = self._assert_new_hit(seq, "fitting_active_ship_guard", "StatsPanel.UpdateStats", "caught:KeyError:")
            sp.UpdateDroneStats()
            seq = self._assert_new_hit(seq, "fitting_active_ship_guard", "StatsPanel.UpdateDroneStats", "caught:KeyError:")
            sp.destroyed = True
            sp.UpdateStats()
            seq = self._assert_new_hit(seq, "fitting_active_ship_guard", "StatsPanel.UpdateStats", "panel_destroyed")
            dp = FakeDronePanel()
            dp.UpdateDroneStats()
            seq = self._assert_new_hit(seq, "fitting_active_ship_guard", "DronePanel.UpdateDroneStats", "caught:KeyError:")

            # #7 base_hangar_behaviour_guard
            bhb = FakeBaseHangarShipBehaviour()
            self.assertFalse(bhb.SetShipDamage(0, "m"))
            seq = self._assert_new_hit(seq, "base_hangar_behaviour_guard", "BaseHangarShipBehaviour.SetShipDamage", "unset_item_or_undocked")
            self.assertFalse(bhb.SetShipDamage(9999, "m"))
            seq = self._assert_new_hit(seq, "base_hangar_behaviour_guard", "BaseHangarShipBehaviour.SetShipDamage", "stale_ship_id")

            # #8 modular_capital_hangar_guard
            cap_beh = FakeModularCapitalHangarShipBehaviour()
            cap_beh.ProcessSkinChange("m")
            seq = self._assert_new_hit(seq, "modular_capital_hangar_guard", "ModularCapitalHangarShipBehaviour.ProcessSkinChange", "unset_player_ship_item_id")
            cap_beh.playerShipItemID = 9999
            cap_beh.ProcessSkinChange("m")
            seq = self._assert_new_hit(seq, "modular_capital_hangar_guard", "ModularCapitalHangarShipBehaviour.ProcessSkinChange", "stale_player_ship_item_id")

            # #9 modular_hangar_teardown_guard
            mhb = FakeModularHangarShipBehaviour()
            mhb.CleanScene()
            seq = self._assert_new_hit(seq, "modular_hangar_teardown_guard", "ModularHangarShipBehaviour.CleanScene", "sanitized_null_camera_or_docks")
            mhb._raise_clear_refs = True
            mhb._cam = None
            mhb.smallDocks = []
            mhb.bigDocks = []
            mhb.CleanScene()
            seq = self._assert_new_hit(seq, "modular_hangar_teardown_guard", "ModularHangarShipBehaviour.CleanScene", "caught:AttributeError:")
            mhb.ProcessSkinChange("m")
            seq = self._assert_new_hit(seq, "modular_hangar_teardown_guard", "ModularHangarShipBehaviour.ProcessSkinChange", "unset_player_ship_item_id")
            mhb.playerShipItemID = 9999
            mhb.ProcessSkinChange("m")
            seq = self._assert_new_hit(seq, "modular_hangar_teardown_guard", "ModularHangarShipBehaviour.ProcessSkinChange", "stale_player_ship_item_id")

            # #10 wreck_rotation_guard
            FakeSpaceMgr().LoadObject("w")
            wr = FakeWreck()
            wr.Prepare()
            seq = self._assert_new_hit(seq, "wreck_rotation_guard", "Wreck.Prepare", "caught:AttributeError:model_is_none:")
            wr.SetRandomRotation()
            seq = self._assert_new_hit(seq, "wreck_rotation_guard", "Wreck.SetRandomRotation", "caught:AttributeError:model_is_none:")
            wr.SetDungeonRotation()
            seq = self._assert_new_hit(seq, "wreck_rotation_guard", "Wreck.SetDungeonRotation", "caught:AttributeError:model_is_none:")
            wr.SetBallRotation()
            seq = self._assert_new_hit(seq, "wreck_rotation_guard", "Wreck.SetBallRotation", "caught:AttributeError:model_is_none:")

            # #11 slots_container_sync_guard
            sc = FakeSlotsContainer()
            sc.OnModuleFitted()
            seq = self._assert_new_hit(seq, "slots_container_sync_guard", "SlotsContainer.OnModuleFitted", "injected_on_module_fitted_refresh")
            sc.ProcessShipEffect(None, types.SimpleNamespace(itemID=7001))
            seq = self._assert_new_hit(seq, "slots_container_sync_guard", "SlotsContainer.ProcessShipEffect", "missing_module_slot_refresh")
            sc.OnInvChange(types.SimpleNamespace(locationID=2002, itemID=7002, flagID=27), {})
            seq = self._assert_new_hit(seq, "slots_container_sync_guard", "SlotsContainer.OnInvChange", "missing_module_slot_refresh")

            # #12 sensor_suite_gfx_guard
            gfx = FakeGfxHandler()
            gfx.StartGfxSwipe()
            seq = self._assert_new_hit(seq, "sensor_suite_gfx_guard", "SensorSuiteHandler.StartGfxSwipe", "cannot_run_sensor_gfx_swipe")
            sess_in_space = _make_fake_session(stationid=None, structureid=None, solarsystemid=30000142)
            builtins.session = sess_in_space
            gfx.sceneManager = types.SimpleNamespace(GetRegisteredScene=lambda name: "default_scene")
            gfx.StartGfxSwipe()
            seq = self._assert_new_hit(seq, "sensor_suite_gfx_guard", "SensorSuiteHandler.StartGfxSwipe", "caught:RuntimeError:")
            gfx._GfxSwipeThread()
            seq = self._assert_new_hit(seq, "sensor_suite_gfx_guard", "SensorSuiteHandler._GfxSwipeThread", "caught:RuntimeError:")
            builtins.session = sess

            # #13 modular_hangar_camera_guard
            mc = FakeModularHangarCamera()
            mc.AnimEnterHangar(None)
            seq = self._assert_new_hit(seq, "modular_hangar_camera_guard", "ModularHangarCamera.AnimEnterHangar", "model_is_none")
            mc.CalculateTargetPoint()
            seq = self._assert_new_hit(seq, "modular_hangar_camera_guard", "ModularHangarCamera.CalculateTargetPoint", "model_is_none")
            self.assertEqual(mc.GetSizeDependantZoomConstants(), (1450.0, 250.0, 500.0))
            seq = self._assert_new_hit(seq, "modular_hangar_camera_guard", "ModularHangarCamera.GetSizeDependantZoomConstants", "model_is_none")
            mc._updateShipChangeLocations()
            seq = self._assert_new_hit(seq, "modular_hangar_camera_guard", "ModularHangarCamera._updateShipChangeLocations", "model_is_none")

            mcc = FakeModularHangarCapitalCamera()
            mcc.AnimEnterHangar(None)
            seq = self._assert_new_hit(seq, "modular_hangar_camera_guard", "ModularHangarCapitalCamera.AnimEnterHangar", "model_is_none")
            self.assertEqual(mcc.CalculateOrbitTarget(), (3150.0, 2220.0, 16500.0))
            seq = self._assert_new_hit(seq, "modular_hangar_camera_guard", "ModularHangarCapitalCamera.CalculateOrbitTarget", "model_is_none")
            mcc.Orbit()
            seq = self._assert_new_hit(seq, "modular_hangar_camera_guard", "ModularHangarCapitalCamera.Orbit", "model_is_none")
            self.assertEqual(mcc._configureCameraStartLocations(670, wasOrbiting=True), (670, False))
            seq = self._assert_new_hit(seq, "modular_hangar_camera_guard", "ModularHangarCapitalCamera._configureCameraStartLocations", "downgraded_was_orbiting")
            mcc.CapitalOrbitUpdateThread()
            seq = self._assert_new_hit(seq, "modular_hangar_camera_guard", "ModularHangarCapitalCamera.CapitalOrbitUpdateThread", "model_is_none_not_lerping")
            mcc.lerpingIntoPosition = True
            mcc.CapitalOrbitUpdateThread()
            seq = self._assert_new_hit(seq, "modular_hangar_camera_guard", "ModularHangarCapitalCamera.CapitalOrbitUpdateThread", "caught:AttributeError:")

            # #14 skill_plan_panel_signal_guard
            spp = FakeSkillPlanPanel()
            spp.destroyed = True
            spp.OnSkillPlanCreated()
            seq = self._assert_new_hit(seq, "skill_plan_panel_signal_guard", "SkillPlanPanel.OnSkillPlanCreated", "panel_destroyed")
            spp.OnCreateNewPersonalSkillPlanButton()
            seq = self._assert_new_hit(seq, "skill_plan_panel_signal_guard", "SkillPlanPanel.OnCreateNewPersonalSkillPlanButton", "panel_destroyed")
            spp.Close()

            # #15 probe_tracker_guard
            pt = FakeProbeTracker()
            self.assertEqual(pt.OnProbesIdle([{"probeID": 5001}, {"probeID": 9999}]), 1)
            seq = self._assert_new_hit(seq, "probe_tracker_guard", "ProbeTracker.OnProbesIdle", "filtered_unknown_probes:1")
            pt.SetProbeDestination(9999, (0, 0, 0))
            seq = self._assert_new_hit(seq, "probe_tracker_guard", "ProbeTracker.SetProbeDestination", "unknown_probe_id")
            pt.UpdateProbeState(9999, 2)
            seq = self._assert_new_hit(seq, "probe_tracker_guard", "ProbeTracker.UpdateProbeState", "unknown_probe_id")

            # Also verify caught:KeyError fallback on ProbeTracker methods if probeData vanishes mid-call:
            class DropProbeTracker(FakeProbeTracker):
                def OnProbesIdle(self, probes):
                    self.probeData = {}
                    raise KeyError(5001)

                def SetProbeDestination(self, probeID, pos):
                    self.probeData = {}
                    raise KeyError(probeID)

            evomu_site._HOOKED_CAMERA_GUARD_TARGETS.discard("probeTracker")
            sys.modules["probescanning.probeTracker"].ProbeTracker = DropProbeTracker
            evomu_site._install_probe_tracker_guard()
            dpt = DropProbeTracker()
            dpt.OnProbesIdle([{"probeID": 5001}])
            seq = self._assert_new_hit(seq, "probe_tracker_guard", "ProbeTracker.OnProbesIdle", "caught:KeyError:")
            dpt.probeData = {5001: {}}
            dpt.SetProbeDestination(5001, (0, 0, 0))
            self._assert_new_hit(seq, "probe_tracker_guard", "ProbeTracker.SetProbeDestination", "caught:KeyError:")

            # Verify all 15 guard IDs recorded at least one hit across Tests 1-15:
            # Summary check here covers guards #6-#15 (and Tests 1, 2, 3/4, 5 cover #1-#5).
            seen = set(evomu_site.get_guard_hit_summary()["guard_hits_by_guard"].keys())
            for gid in (
                "fitting_active_ship_guard",
                "base_hangar_behaviour_guard",
                "modular_capital_hangar_guard",
                "modular_hangar_teardown_guard",
                "wreck_rotation_guard",
                "slots_container_sync_guard",
                "sensor_suite_gfx_guard",
                "modular_hangar_camera_guard",
                "skill_plan_panel_signal_guard",
                "probe_tracker_guard",
            ):
                self.assertIn(gid, seen)


class TestOffSwitchAndOutOfScopeAdapters(unittest.TestCase):
    """Verify EVOMU_DISABLE_SERVER_GUARDS=1 disables all 15 server-caused guards but leaves adapters on."""

    def test_off_switch_prevents_all_server_caused_guards_even_via_meta_hook(self):
        class DummyCls:
            def _OpenView(self): pass
            def ActivateView(self): pass
            def ResetAnchorPos(self): pass
            def UpdateAnchorOffset(self): pass
            def GetTrackPosition(self): pass
            def StartTransition(self): pass
            def EndTransition(self): pass
            def ClearScene(self): pass
            def ReplaceExistingShipModel(self): pass
            def _IsCurrentInventoryValid(self): pass
            def GetFittingDictForActiveShip(self): pass
            def SetShipDamage(self): pass
            def ProcessSkinChange(self): pass
            def CleanScene(self): pass
            def Prepare(self): pass
            def LoadObject(self): pass
            def ProcessShipEffect(self): pass
            def StartGfxSwipe(self): pass
            def AnimEnterHangar(self): pass
            def Close(self): pass
            def OnProbesIdle(self): pass

        with _isolated_evomu_site_env(
            env_overrides={guard_registry.DISABLE_SERVER_GUARDS_ENV: "1"}
        ):
            self.assertTrue(evomu_site.is_server_guards_disabled())
            for mod_name, attrs in (
                ("eve.client.script.ui.services.viewStateSvc", {"ViewStateSvc": DummyCls}),
                ("eve.client.script.ui.camera.baseSpaceCamera", {"BaseSpaceCamera": DummyCls}),
                ("eve.client.script.ui.view.transitions", {"HangarToHangarTransition": DummyCls}),
                ("eve.client.script.ui.view.hangarView", {"HangarView": DummyCls}),
                ("eve.client.script.ui.shared.inventory.invWindow", {"Inventory": DummyCls}),
                ("eve.client.script.environment.fittingSvc", {"FittingSvc": DummyCls}),
                ("eve.client.script.ui.view.hangarBehaviours.baseHangarBehaviours", {"BaseHangarShipBehaviour": DummyCls}),
                ("eve.client.script.ui.view.hangarBehaviours.modularHangarCapitalBehaviours", {"ModularCapitalHangarShipBehaviour": DummyCls}),
                ("eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours", {"ModularHangarShipBehaviour": DummyCls}),
                ("eve.client.script.environment.spaceObject.wreck", {"Wreck": DummyCls}),
                ("eve.client.script.parklife.spaceMgr", {"SpaceMgr": DummyCls}),
                ("eve.client.script.ui.inflight.shipHud.slotsContainer", {"SlotsContainer": DummyCls}),
                ("sensorsuite.overlay.gfxhandler", {"GfxHandler": DummyCls}),
                ("eve.client.script.ui.camera.modularHangarCamera", {"ModularHangarCamera": DummyCls}),
                ("eve.client.script.ui.skillPlan.skillPlanPanel", {"SkillPlanPanel": DummyCls}),
                ("probescanning.probeTracker", {"ProbeTracker": DummyCls}),
            ):
                _register_fake_module(mod_name, **attrs)

            uthread_calls = []
            _register_fake_module("uthread", new=lambda fn: uthread_calls.append(fn))

            for entry in guard_registry.BEHAVIOUR_GUARDS:
                inst_fn = getattr(evomu_site, entry["installer"])
                res = inst_fn()
                self.assertFalse(res, f"{entry['installer']} should return False when off-switch is set")

            evomu_site._install_camera_dock_guard()
            evomu_site._install_live_client_sync_guards()

            meta_hook = evomu_site._EvomuMetaHook()
            for mod_name in (
                "eve.client.script.ui.camera.baseSpaceCamera",
                "eve.client.script.environment.spaceObject.wreck",
                "eve.client.script.ui.skillPlan.skillPlanPanel",
                "eve.client.script.environment.invCache",
                "probescanning.probeTracker",
                "shipfitting.fittingWarnings",
                "eve.client.script.ui.shared.fittingScreen.warningIconCont",
                "eve.client.script.ui.shared.fittingScreen.statsPanel",
                "eve.client.script.ui.shared.fittingScreen.fittingPanels.dronePanel",
            ):
                meta_hook.find_module(mod_name)

            self.assertEqual(uthread_calls, [])
            self.assertEqual(evomu_site._HOOKED_CAMERA_GUARD_TARGETS, set())
            for attr_name in dir(DummyCls):
                if not attr_name.startswith("__"):
                    fn = getattr(DummyCls, attr_name)
                    if callable(fn):
                        self.assertFalse(getattr(fn, "_evomu_camera_guard", False), attr_name)

            summary = evomu_site.get_guard_hit_summary()
            self.assertTrue(summary["client_guards_off"])
            self.assertEqual(summary["guard_hits_count"], 0)

    def test_off_switch_preserves_out_of_scope_adapters(self):
        with _isolated_evomu_site_env(
            env_overrides={guard_registry.DISABLE_SERVER_GUARDS_ENV: "1"}
        ):
            prefs_set = []
            _register_fake_module(
                "logmodule",
                LogException=lambda *a, **k: "orig_exc",
                LogTraceback=lambda *a, **k: "orig_tb",
                LogError=lambda *a, **k: "orig_err",
                LogWarn=lambda *a, **k: "orig_warn",
            )
            _register_fake_module(
                "eveprefs",
                prefs=types.SimpleNamespace(SetValue=lambda k, v: prefs_set.append((k, v))),
            )

            class FakeXmppConnection:
                def __init__(self):
                    self.port = 5222

            _register_fake_module("xmpp.xmppConnection", XmppConnection=FakeXmppConnection)

            class FakeFlatbufferProvider:
                def retrieve_data_from_cdn(self, url, crc):
                    return "data"

                def _send_http_request(self, host, req, timeout_seconds=10):
                    return b"ok"

            _register_fake_module(
                "content.client.flatbuffer.provider",
                FlatbufferContentProvider=FakeFlatbufferProvider,
                separate_host_and_request=lambda url: (url, "/"),
            )

            class FakeBuiltDataLoader:
                @classmethod
                def GetData(cls):
                    return {}

            _register_fake_module(
                "fsdBuiltData.common.base",
                BuiltDataLoader=FakeBuiltDataLoader,
            )

            evomu_site._install_logmodule_hook()
            evomu_site._install_xmpp_plaintext_guard()
            evomu_site._install_xmpp_port_guard()
            evomu_site._install_flatbuffer_cdn_http_guard()
            evomu_site._install_fsd_typelist_preload_hook(preload_typelist=False)

            self.assertTrue(getattr(sys.modules["logmodule"].LogException, "_evomu_wrapped", False))
            self.assertIn(("chatserver_disable_tls", 1), prefs_set)
            self.assertTrue(getattr(FakeXmppConnection.__init__, "_evomu_xmpp_port_patched", False))
            self.assertTrue(getattr(FakeFlatbufferProvider._send_http_request, "_evomu_flatbuffer_cdn_patched", False))
            self.assertTrue(getattr(FakeBuiltDataLoader, "_evomu_getdata_patched", False))

    def test_approved_client_only_guard_stays_enabled_and_unapproved_stays_off(self):
        class FakeBaseHangar:
            def SetShipDamage(self, itemID, model):
                return True

        class FakeCapHangar:
            playerShipItemID = None

            def ProcessSkinChange(self, newModel):
                return True

        sess = _make_fake_session(stationid=60003760, shipid=2002)
        approved_entry = dict(guard_registry.BEHAVIOUR_GUARDS_BY_ID["base_hangar_behaviour_guard"])
        approved_entry["classification"] = guard_registry.CLASSIFICATION_CLIENT_ONLY
        approved_entry["client_only_case"] = "Pure client UI damage texture race without server packet"
        approved_entry["owner_approved"] = True

        # Client-only WITHOUT owner_approved=True must remain disabled when EVOMU_DISABLE_SERVER_GUARDS=1.
        unapproved_entry = dict(guard_registry.BEHAVIOUR_GUARDS_BY_ID["modular_capital_hangar_guard"])
        unapproved_entry["classification"] = guard_registry.CLASSIFICATION_CLIENT_ONLY
        unapproved_entry["client_only_case"] = "Claimed client-only without owner sign-off"
        unapproved_entry["owner_approved"] = False

        patched_by_id = dict(guard_registry.BEHAVIOUR_GUARDS_BY_ID)
        patched_by_id["base_hangar_behaviour_guard"] = approved_entry
        patched_by_id["modular_capital_hangar_guard"] = unapproved_entry

        with _isolated_evomu_site_env(
            env_overrides={guard_registry.DISABLE_SERVER_GUARDS_ENV: "1"},
            session_obj=sess,
        ), mock.patch.dict(guard_registry.BEHAVIOUR_GUARDS_BY_ID, patched_by_id, clear=True):
            _register_fake_module(
                "eve.client.script.ui.view.hangarBehaviours.baseHangarBehaviours",
                BaseHangarShipBehaviour=FakeBaseHangar,
            )
            _register_fake_module(
                "eve.client.script.ui.view.hangarBehaviours.modularHangarCapitalBehaviours",
                ModularCapitalHangarShipBehaviour=FakeCapHangar,
            )
            self.assertTrue(evomu_site._install_base_hangar_behaviour_guard())
            self.assertFalse(evomu_site._install_modular_capital_hangar_guard())

            FakeBaseHangar().SetShipDamage(0, "m")
            summary = evomu_site.get_guard_hit_summary()
            self.assertTrue(summary["client_guards_off"])
            self.assertEqual(summary["guard_hits_count"], 1)
            self.assertEqual(summary["guard_hits_by_guard"], {"base_hangar_behaviour_guard": 1})
            self.assertEqual(summary["client_only_guard_hits"], 1)
            self.assertEqual(
                summary["client_only_guard_hits_by_guard"],
                {"base_hangar_behaviour_guard": 1},
            )
            self.assertEqual(summary["server_caused_guard_hits"], 0)


class TestTelemetryPropagationEndToEnd(unittest.TestCase):
    """Verify guard telemetry propagates through hook, playtester runner, ScenarioRunner, and run_journeys."""

    def test_evomu_playtester_hook_action_and_rpc_telemetry(self):
        import socket

        hook = _load_py2_harness_module("evomu_playtester_hook.py", stub_helpers=True)
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe_sock:
            probe_sock.bind(("127.0.0.1", 0))
            free_port = probe_sock.getsockname()[1]
        with _isolated_evomu_site_env(
            env_overrides={
                guard_registry.DISABLE_SERVER_GUARDS_ENV: "0",
                "EVOMU_PLAYTESTER_PORT": "",
            }
        ):
            evomu_site._record_guard_hit("camera_dock_guard", "Boot.Init", "pre_step_hit")
            fake_sm = mock.Mock()
            fake_sm.IsServiceRunning.return_value = False
            srv = hook.PlaytesterRpcServer({"playtester_port": free_port}, fake_sm)
            try:
                snap = {
                    "open_windows": [],
                    "modals": [],
                    "ui": {},
                    "space": {},
                    "skills_state": {},
                    "wallet_state": {},
                    "active_view": "hangar",
                }
                with (
                    mock.patch.object(hook, "_install_flight_recorder_hooks"),
                    mock.patch.object(hook, "_install_warp_telemetry"),
                    mock.patch.object(hook, "_get_state_snapshot", return_value=snap),
                    mock.patch.object(hook, "_compute_state_diff", return_value={"ui_changed": False, "view_changed": False}),
                    mock.patch.object(hook, "_settle_secondary_view_transition"),
                    mock.patch.object(hook.H, "_sleep_sim_ms", create=True),
                ):
                    def _step1_sleep(_ms):
                        evomu_site._record_guard_hit("wreck_rotation_guard", "Wreck.Prepare", "step1_hit")

                    with mock.patch.object(hook.H, "_sleep_ms", side_effect=_step1_sleep, create=True):
                        res1 = hook._execute_action(fake_sm, {"type": "wait", "wait_ms": 10, "strict_wait": True})

                    def _step2_sleep(_ms):
                        evomu_site._record_guard_hit("probe_tracker_guard", "ProbeTracker.OnProbesIdle", "step2_hit")

                    with mock.patch.object(hook.H, "_sleep_ms", side_effect=_step2_sleep, create=True):
                        res2 = hook._execute_action(fake_sm, {"type": "wait", "wait_ms": 10, "strict_wait": True})

                self.assertEqual(res1["guard_hit_count"], 1)
                self.assertEqual(res1["guard_hits"][0]["guard"], "wreck_rotation_guard")
                self.assertEqual(res1["flight_recorder"]["guard_summary"]["guard_hits_count"], 1)
                self.assertEqual(res1["guard_summary"]["guard_hits_by_guard"], {"wreck_rotation_guard": 1})

                self.assertEqual(res2["guard_hit_count"], 1)
                self.assertEqual(res2["guard_hits"][0]["guard"], "probe_tracker_guard")
                self.assertEqual(res2["guard_summary"]["guard_hits_by_guard"], {"probe_tracker_guard": 1})

                rpc_all = srv._dispatch({"method": "get_guard_hits", "params": {"since_seq": 0}})
                self.assertTrue(rpc_all["ok"])
                self.assertEqual(rpc_all["guard_summary"]["guard_hits_count"], 3)
                self.assertEqual(rpc_all["guard_summary"]["guard_hits_by_guard"], {
                    "camera_dock_guard": 1,
                    "probe_tracker_guard": 1,
                    "wreck_rotation_guard": 1,
                })

                fr_resp = srv._dispatch({"method": "get_flight_recorder", "params": {"since_seq": 0, "since_guard_seq": 1}})
                self.assertEqual(len(fr_resp["guard_hits"]), 2)
                self.assertEqual(fr_resp["guard_summary"]["guard_hits_count"], 2)
            finally:
                try:
                    srv.sock.close()
                except Exception:
                    pass

    def test_playtester_runner_keeps_guards_on_and_records_telemetry(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            mem_dir = root / "runtime"
            run_dir = root / "run_01"
            step_dir = run_dir / "steps" / "step_00001"
            step_dir.mkdir(parents=True, exist_ok=True)
            stage_dir = root / "staged_client"
            (stage_dir / "bin64").mkdir(parents=True, exist_ok=True)
            (stage_dir / "bin64" / "exefile").write_text("#!/bin/sh\n", encoding="utf-8")

            runner = playtester.PlaytesterRunner.__new__(playtester.PlaytesterRunner)
            runner.worktree_dir = root
            runner.runtime_dir = mem_dir
            runner.current_run_dir = run_dir
            runner.staged_app = root / "EVE.app"
            runner.args = types.SimpleNamespace(silent=True)
            runner.ports = {"gateway": 26005, "http": 26004, "macho": 26000, "rpc": 26999}
            runner.client_proc = None
            runner.memory = playtester.PlaytesterMemory(mem_dir, git_head="abc123", server_bin_mtime=1.0)
            runner.write_inclient_cfg = lambda: run_dir / "inclient.json"
            runner._sync_initial_session_flight_calls = lambda boot: None

            launched_envs = []

            class DummyProc:
                pid = 4242
                returncode = None

                def poll(self):
                    return None

            runner.rpc = mock.MagicMock()
            runner.rpc.ping.return_value = True
            runner.rpc.call.side_effect = lambda cmd, params=None, timeout=20.0: (
                {
                    "session": {"charid": 90000001},
                    "active_view": "hangar",
                    "flight_recorder": {
                        "guard_hits": [{"seq": 1, "guard": "camera_dock_guard", "function": "F", "reason": "r", "time": 1.0}],
                        "guard_summary": {"client_guards_off": False},
                    },
                }
                if cmd == "bootstrap_session"
                else {"guard_hits": [], "guard_summary": {"client_guards_off": False}}
            )

            def _fake_popen(cmd, cwd=None, stdout=None, stderr=None, env=None, start_new_session=True):
                launched_envs.append(dict(env or {}))
                return DummyProc()

            with (
                mock.patch.dict(os.environ, {guard_registry.DISABLE_SERVER_GUARDS_ENV: "1"}),
                mock.patch.object(playtester, "stage_playtester_client", return_value=stage_dir),
                mock.patch.object(playtester, "mint_sso_jwt_via_server", return_value="tok"),
                mock.patch.object(playtester.subprocess, "Popen", side_effect=_fake_popen),
            ):
                self.assertTrue(runner.start_client())

            self.assertEqual(len(launched_envs), 1)
            self.assertNotIn(guard_registry.DISABLE_SERVER_GUARDS_ENV, launched_envs[0])

            exec_res = {
                "action_ok": True,
                "guard_hits": [
                    {"seq": 2, "guard": "wreck_rotation_guard", "function": "Wreck.Prepare", "reason": "model_is_none", "time": 2.0}
                ],
                "guard_summary": {"client_guards_off": False},
            }
            runner._record_step_guard_hits(1, exec_res)
            (step_dir / "exec_result.json").write_text(json.dumps(exec_res, indent=2), encoding="utf-8")
            runner.memory.apply_agent_decision(
                1,
                {"action": {"type": "wait", "target": "10"}, "observation": "ok"},
                exec_res,
            )
            runner._write_run_summary(step_num=1, final=True)

            exec_disk = json.loads((step_dir / "exec_result.json").read_text(encoding="utf-8"))
            self.assertEqual(exec_disk["guard_hit_count"], 1)
            self.assertEqual(exec_disk["guard_summary"]["client_guards_off"], False)
            self.assertEqual(exec_disk["guard_summary"]["guard_hits_count"], 2)

            run_sum = json.loads((run_dir / "run_summary.json").read_text(encoding="utf-8"))
            self.assertFalse(run_sum["client_guards_off"])
            self.assertEqual(run_sum["guard_hits_count"], 2)
            self.assertEqual(run_sum["guard_hits_by_guard"], {"camera_dock_guard": 1, "wreck_rotation_guard": 1})
            self.assertTrue(run_sum["final"])

            mem_disk = json.loads((mem_dir / "memory.json").read_text(encoding="utf-8"))
            self.assertEqual(mem_disk["recent_actions"][-1]["guard_hit_count"], 1)
            self.assertEqual(mem_disk["last_run_summary"]["guard_hits_count"], 2)

    def test_scenario_runner_disables_server_guards_and_records_result_json(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            scen_path = root / "scen.json"
            scen_path.write_text(
                json.dumps({
                    "id": "guard_check",
                    "title": "Guard Check Scenario",
                    "steps": [
                        {
                            "id": "s1",
                            "action": {"type": "wait", "target": "10"},
                            "checks": {},
                        }
                    ],
                }),
                encoding="utf-8",
            )
            scen = playtester.load_scenario_file(scen_path)
            out_dir = root / "out"
            run_dir = root / "run"
            run_dir.mkdir(parents=True, exist_ok=True)
            (run_dir / "client_logs").mkdir(parents=True, exist_ok=True)
            (run_dir / "server.log").write_text("", encoding="utf-8")

            stage_dir = root / "staged_client"
            (stage_dir / "bin64").mkdir(parents=True, exist_ok=True)
            (stage_dir / "bin64" / "exefile").write_text("#!/bin/sh\n", encoding="utf-8")

            approved_entry = dict(guard_registry.BEHAVIOUR_GUARDS_BY_ID["base_hangar_behaviour_guard"])
            approved_entry["classification"] = guard_registry.CLASSIFICATION_CLIENT_ONLY
            approved_entry["client_only_case"] = "Approved test case"
            approved_entry["owner_approved"] = True

            runner = playtester.ScenarioRunner.__new__(playtester.ScenarioRunner)
            runner.worktree_dir = root
            runner.current_run_dir = run_dir
            runner.out_dir = out_dir
            out_dir.mkdir(parents=True, exist_ok=True)
            runner.staged_app = root / "EVE.app"
            runner.world_db_path = root / "missing_world.sqlite"
            runner.args = types.SimpleNamespace(silent=True)
            runner.ports = {"gateway": 26005, "http": 26004, "macho": 26000, "rpc": 26998}
            runner.client_proc = None
            runner.server_proc = None
            runner._gm_bridge_conn = None
            runner._gm_bridge_sm = None
            runner._session_flight_calls = []
            runner._session_flight_seqs = set()
            runner._active_declared_setup_steps = []
            runner._active_expected_characters = []
            runner._active_starting_state = {}
            runner._last_boot_result = {}
            runner._scenario_auth_params = lambda: ("demo", "demo-secret", 90000001, "Demo Pilot")
            runner.write_inclient_cfg = lambda: run_dir / "inclient.json"
            runner.stop_client = lambda: None
            runner.stop_server = lambda: None
            runner._close_gm_bridge = lambda: None
            runner.start_server = lambda *a, **kw: True
            runner._ensure_starting_state = lambda scen_cfg: []
            runner._record_session_flight_calls = lambda calls: None
            runner._capture_window_screenshot = lambda *a, **k: None

            launched_envs = []

            class DummyProc:
                pid = 5151
                returncode = None

                def poll(self):
                    return None

            runner.rpc = mock.MagicMock()
            runner.rpc.ping.return_value = True

            def _rpc_handler(cmd, params=None, timeout=20.0):
                if cmd == "bootstrap_session":
                    return {
                        "ok": True,
                        "session": {"charid": 90000001},
                        "active_view": "hangar",
                        "flight_recorder": {
                            "macho_calls": [],
                            "guard_hits": [],
                            "guard_summary": {"client_guards_off": True},
                        },
                    }
                if cmd == "execute_action":
                    return {
                        "action_ok": True,
                        "post_state": {"client_build": "9990001", "session": {"charid": 90000001}},
                        "flight_recorder": {
                            "macho_calls": [],
                            "scatters_observed": [],
                            "log_exceptions": [],
                            "quasar_errors": [],
                            "macho_exceptions": [],
                        },
                        "guard_hits": [
                            {
                                "seq": 1,
                                "guard": "base_hangar_behaviour_guard",
                                "function": "BaseHangarShipBehaviour.SetShipDamage",
                                "reason": "stale_ship_id",
                                "time": 10.0,
                            }
                        ],
                        "guard_summary": {"client_guards_off": True},
                    }
                if cmd == "get_guard_hits":
                    return {"ok": True, "guard_hits": [], "guard_summary": {"client_guards_off": True}}
                return {"ok": True, "macho_calls": []}

            runner.rpc.call.side_effect = _rpc_handler

            def _fake_popen(cmd, cwd=None, stdout=None, stderr=None, env=None, start_new_session=True):
                launched_envs.append(dict(env or {}))
                return DummyProc()

            with (
                mock.patch.dict(os.environ, {}, clear=False),
                mock.patch.dict(guard_registry.BEHAVIOUR_GUARDS_BY_ID, {"base_hangar_behaviour_guard": approved_entry}),
                mock.patch.object(playtester, "stage_playtester_client", return_value=stage_dir),
                mock.patch.object(playtester, "mint_sso_jwt_via_server", return_value="tok"),
                mock.patch.object(playtester.subprocess, "Popen", side_effect=_fake_popen),
            ):
                os.environ.pop(guard_registry.DISABLE_SERVER_GUARDS_ENV, None)
                orig_start = runner.start_client

                def _mock_start_client():
                    ok = playtester.ScenarioRunner.start_client(runner)
                    if ok:
                        runner._last_boot_result = runner.rpc.call("bootstrap_session", {})
                    return ok

                runner.start_client = _mock_start_client
                runner.scenario_paths = [scen_path]
                rc = runner.run_scenarios()

            self.assertEqual(rc, 0)
            self.assertEqual(len(launched_envs), 1)
            self.assertEqual(launched_envs[0].get(guard_registry.DISABLE_SERVER_GUARDS_ENV), "1")

            res = json.loads((out_dir / "result.json").read_text(encoding="utf-8"))
            self.assertTrue(res["client_guards_off"])
            self.assertEqual(res["guard_hits_count"], 1)
            self.assertEqual(res["guard_hits_by_guard"], {"base_hangar_behaviour_guard": 1})
            self.assertEqual(res["client_only_guard_hits"], 1)
            self.assertEqual(res["client_only_guard_hits_by_guard"], {"base_hangar_behaviour_guard": 1})
            j = res["journeys"]["guard_check"]
            self.assertTrue(j["client_guards_off"])
            self.assertEqual(j["client_only_guard_hits"], 1)
            self.assertEqual(j["steps"][0]["guard_hits_count"], 1)

    def test_inclient_driver_and_run_journeys_attestation_telemetry(self):
        drv = _load_py2_harness_module("evomu_inclient_driver.py", stub_helpers=False)
        rj = _load_run_journeys_module()
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            stage_dir = root / "stage"
            exe_path = stage_dir / "bin64" / "exefile"
            exe_path.parent.mkdir(parents=True, exist_ok=True)
            exe_path.write_text("#!/bin/sh\n", encoding="utf-8")

            launched_envs = []

            class DummyProc:
                pid = 6161

            with (
                mock.patch.object(rj, "stage_real_client", return_value=(str(stage_dir), str(exe_path))),
                mock.patch.object(rj, "_pids_matching", return_value=set()),
                mock.patch.object(rj.subprocess, "Popen", side_effect=lambda *a, **kw: (launched_envs.append(dict(kw.get("env") or {})), DummyProc())[1]),
            ):
                rj.launch_real_client(
                    client_app="/tmp/EVE.app",
                    host="127.0.0.1",
                    macho_port=26000,
                    login="demo",
                    log_path=str(root / "cli.log"),
                )
            self.assertEqual(launched_envs[0].get(guard_registry.DISABLE_SERVER_GUARDS_ENV), "1")

            approved_entry = dict(guard_registry.BEHAVIOUR_GUARDS_BY_ID["base_hangar_behaviour_guard"])
            approved_entry["classification"] = guard_registry.CLASSIFICATION_CLIENT_ONLY
            approved_entry["client_only_case"] = "Approved test case"
            approved_entry["owner_approved"] = True

            sess = _make_fake_session(stationid=60003760, shipid=2002)
            sm = _make_fake_sm({
                "publicGatewaySvc": types.SimpleNamespace(),
                "stationSvc": types.SimpleNamespace(),
                "viewState": types.SimpleNamespace(GetCurrentViewInfo=lambda: types.SimpleNamespace(name="hangar")),
            })
            ipc_msgs = []
            fake_ipc = types.SimpleNamespace(
                notify=lambda p: ipc_msgs.append(p),
                request=lambda p, timeout=None: {"ok": True},
            )

            with (
                _isolated_evomu_site_env(
                    env_overrides={guard_registry.DISABLE_SERVER_GUARDS_ENV: "1"},
                    session_obj=sess,
                    sm_obj=sm,
                ),
                mock.patch.dict(guard_registry.BEHAVIOUR_GUARDS_BY_ID, {"base_hangar_behaviour_guard": approved_entry}),
                mock.patch.object(drv, "_wait_for_session", return_value=90000001),
                mock.patch.object(drv, "_sync_quasar_gateway", return_value=types.SimpleNamespace()),
                mock.patch.object(drv, "_install_rpc_recorder", return_value=[None]),
                mock.patch.object(drv, "_snapshot_ship_fitted_item_ids", return_value=set()),
                mock.patch.object(drv, "_reset_baseline_state", return_value=None),
                mock.patch.object(drv.__builtin__, "sm", sm, create=True),
                mock.patch.object(drv.__builtin__, "session", sess, create=True),
            ):
                def _fake_launch(ctx):
                    evomu_site._record_guard_hit("base_hangar_behaviour_guard", "BaseHangarShipBehaviour.SetShipDamage", "stale_ship_id")
                    return (True, {"bridge_verified": True})

                def _fake_station(ctx):
                    return (True, {"bridge_verified": True})

                drv_out = root / "driver_out"
                with mock.patch.object(
                    drv,
                    "_JOURNEY_HANDLERS",
                    [("launch_login", _fake_launch), ("station_ui", _fake_station)],
                ):
                    outcomes = drv._execute_all_journeys(
                        {
                            "out_dir": str(drv_out),
                            "only_journeys": ["launch_login", "station_ui"],
                            "char_id": 90000001,
                        },
                        fake_ipc,
                    )

                self.assertTrue(outcomes["launch_login"]["client_guards_off"])
                self.assertEqual(outcomes["launch_login"]["guard_hits_count"], 1)
                self.assertEqual(outcomes["launch_login"]["client_only_guard_hits"], 1)
                self.assertEqual(outcomes["station_ui"]["guard_hits_count"], 0)

                srv_log = root / "server.log"
                srv_log.write_text("", encoding="utf-8")
                cli_log = root / "client.log"
                cli_log.write_text(
                    "[EVOMU_JOURNEY_START] id=launch_login\n"
                    "[EVOMU_JOURNEY_END] id=launch_login passed=True rpcs=1\n"
                    "[EVOMU_JOURNEY_START] id=station_ui\n"
                    "[EVOMU_JOURNEY_END] id=station_ui passed=True rpcs=1\n",
                    encoding="utf-8",
                )
                rj_out = root / "rj_out"
                rj_out.mkdir(parents=True, exist_ok=True)
                result, rc = rj.finalize_journey_run(
                    str(rj_out),
                    [{"id": "launch_login", "title": "Launch"}, {"id": "station_ui", "title": "Station"}],
                    str(srv_log),
                    [str(cli_log)],
                    "/tmp/EVE.app",
                    {"build": "9990001", "version": "V24.02"},
                    1,
                    True,
                    outcomes,
                    {},
                )

                self.assertEqual(rc, 0)
                self.assertTrue(result["client_guards_off"])
                self.assertEqual(result["guard_hits_count"], 1)
                self.assertEqual(result["guard_hits_by_guard"], {"base_hangar_behaviour_guard": 1})
                self.assertEqual(result["client_only_guard_hits"], 1)
                self.assertEqual(result["client_only_guard_hits_by_guard"], {"base_hangar_behaviour_guard": 1})
                self.assertEqual(result["journeys"]["launch_login"]["client_only_guard_hits"], 1)
                self.assertEqual(result["journeys"]["station_ui"]["guard_hits_count"], 0)


if __name__ == "__main__":
    unittest.main()
