#!/usr/bin/env python3
"""
Offline Python 3 unit tests for EvoMu in-client playtester hook, evomu_site log/FSD hooks,
window-command table validation against client_identifiers.json, and in-client helpers.
Zero cargo builds and zero live EVE server/client processes required.
"""

from __future__ import annotations

import ast
import builtins
import importlib.util
import json
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

HARNESS_DIR = Path(__file__).resolve().parent
REPO_ROOT = HARNESS_DIR.parent.parent
if str(HARNESS_DIR) not in sys.path:
    sys.path.insert(0, str(HARNESS_DIR))



def _load_hook_with_py2_stubs():
    """Load evomu_playtester_hook.py in Python 3 using lightweight stubs for Python 2.7/EVE APIs."""
    hook_path = HARNESS_DIR / "evomu_playtester_hook.py"
    orig_builtin = sys.modules.get("__builtin__")
    orig_helpers = sys.modules.get("evomu_inclient_helpers")
    sys.modules["__builtin__"] = builtins
    fake_helpers = types.ModuleType("evomu_inclient_helpers")
    sys.modules["evomu_inclient_helpers"] = fake_helpers
    try:
        spec = importlib.util.spec_from_file_location("_evomu_playtester_hook_test", str(hook_path))
        mod = importlib.util.module_from_spec(spec)
        assert spec is not None and spec.loader is not None
        mod.__dict__.setdefault("unicode", type("_Py2UnicodeStub", (), {}))
        mod.__dict__.setdefault("basestring", str)
        spec.loader.exec_module(mod)
        return mod
    finally:
        if orig_builtin is not None:
            sys.modules["__builtin__"] = orig_builtin
        else:
            sys.modules.pop("__builtin__", None)
        if orig_helpers is not None:
            sys.modules["evomu_inclient_helpers"] = orig_helpers
        else:
            sys.modules.pop("evomu_inclient_helpers", None)


def _load_helpers_with_py2_stubs():
    """Load evomu_inclient_helpers.py in Python 3 using __builtin__ -> builtins stub."""
    helpers_path = HARNESS_DIR / "evomu_inclient_helpers.py"
    with mock.patch.dict(sys.modules, {"__builtin__": builtins}):
        spec = importlib.util.spec_from_file_location("_evomu_inclient_helpers_test", str(helpers_path))
        mod = importlib.util.module_from_spec(spec)
        assert spec is not None and spec.loader is not None
        spec.loader.exec_module(mod)
        return mod



class TestHookStateDiffAndWindowCleanup(unittest.TestCase):
    def test_hook_state_diff_detects_solar_system_change_and_editable_detection(self):
        hook = _load_hook_with_py2_stubs()
        pre = {
            "session": {"solarsystemid2": 30000139, "solarsystemid": 30000139},
            "active_view": "inflight",
            "open_windows": [],
            "modals": [],
            "skills_state": {},
            "wallet_state": {},
            "space": {"in_space": True, "ego": {"mode": 2, "speed_mps": 0.0}},
            "ui": {"clickable_widgets": []},
        }
        post = {
            "session": {"solarsystemid2": 30000144, "solarsystemid": 30000144},
            "active_view": "inflight",
            "open_windows": [],
            "modals": [],
            "skills_state": {},
            "wallet_state": {},
            "space": {"in_space": True, "ego": {"mode": 2, "speed_mps": 0.0}},
            "ui": {"clickable_widgets": []},
        }
        diff = hook._compute_state_diff(pre, post, scatters=[])
        self.assertEqual(diff["solar_system_changed"], {"from": 30000139, "to": 30000144})
        self.assertTrue(diff["ui_changed"])

        class SingleLineEditText:
            def __init__(self):
                self.value = "Tritanium"
            def SetValue(self, v):
                self.value = v
            def GetValue(self):
                return self.value
            def OnChar(self, ch):
                pass

        class Container:
            pass

        edit_node = SingleLineEditText()
        self.assertTrue(hook._is_editable_input_node(edit_node))
        self.assertTrue(hook._has_real_click_handler(edit_node))
        self.assertEqual(hook._get_editable_value_preview(edit_node), "Tritanium")
        self.assertFalse(hook._is_editable_input_node(Container()))

    def test_hook_cleanup_utility_windows_preserves_modals_hud_and_chat(self):
        hook = _load_hook_with_py2_stubs()

        class FakeWindow:
            def __init__(self, wid, name, cls_name, is_modal=False):
                self.windowID = wid
                self.name = name
                self.destroyed = False
                self.display = True
                self.isModal = is_modal
                self.closed = False
                self.__class__ = type(cls_name, (type(self),), {})
            def CloseByUser(self):
                self.closed = True
                self.destroyed = True

        class FakeStackedWindow(FakeWindow):
            def __init__(self, wid, name, cls_name):
                super().__init__(wid, name, cls_name, is_modal=False)
                self.used_close = False
                self.used_close_by_user = False
            def Close(self):
                self.used_close = True
                self.closed = True
                self.destroyed = True
            def CloseByUser(self):
                self.used_close_by_user = True
                self.closed = True
                self.destroyed = True

        w_modal = FakeWindow("msgbox", "MessageBox", "MessageBox", is_modal=True)
        w_lobby = FakeWindow("lobbyWnd", "lobbyWnd", "LobbyWnd")
        w_chat = FakeWindow("chatchannel_local", "chatchannel_local", "XmppChatWindow")
        w_paint = FakeWindow("PaintTool", "PaintTool", "PaintToolWnd")
        w_agency = FakeWindow("AgencyWndNew", "AgencyWndNew", "AgencyWndNew")
        w_stack = FakeStackedWindow("InventorySpace", "InventorySpace", "InventoryPrimary")

        class FakeReg:
            def GetWindows(self):
                return [w_modal, w_lobby, w_chat, w_paint, w_agency, w_stack]

        fake_uicore = types.SimpleNamespace(registry=FakeReg(), layer=None)
        fake_sm = mock.MagicMock()
        fake_sm.GetService.return_value = None

        with mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            res = hook._cleanup_utility_windows(fake_sm, keep_chat=True)

        self.assertTrue(res["ok"])
        self.assertEqual(set(res["closed_windows"]), {"PaintTool", "AgencyWndNew", "InventorySpace"})
        self.assertTrue(w_paint.closed)
        self.assertTrue(w_agency.closed)
        self.assertTrue(w_stack.used_close)
        self.assertFalse(w_stack.used_close_by_user)
        self.assertFalse(w_modal.closed)
        self.assertFalse(w_lobby.closed)
        self.assertFalse(w_chat.closed)


class TestHookPressKeyCommandFallback(unittest.TestCase):
    def test_press_key_reads_command_and_target(self):
        hook = _load_hook_with_py2_stubs()
        called = []

        class FakeCmdSvc:
            combatFunctionLoaded = None

            def OnEsc(self):
                called.append("OnEsc")

            def OpenFitting(self):
                called.append("OpenFitting")

        fake_uicore = types.SimpleNamespace(cmd=FakeCmdSvc(), registry=None, layer=None)
        fake_sm = mock.MagicMock()

        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"):
            res_cmd = hook._execute_action(
                fake_sm, {"type": "press_key", "target": "", "command": "ESCAPE", "wait_ms": 0}
            )
            res_tgt = hook._execute_action(
                fake_sm, {"type": "press_key", "target": "ALT+F", "command": "", "wait_ms": 0}
            )

        self.assertTrue(res_cmd.get("ok"), res_cmd)
        self.assertEqual(res_cmd.get("executed_cmd"), "OnEsc")
        self.assertTrue(res_tgt.get("ok"), res_tgt)
        self.assertEqual(res_tgt.get("executed_cmd"), "OpenFitting")
        self.assertEqual(called, ["OnEsc", "OpenFitting"])

    def test_press_key_escape_does_not_preclose_context_menu_before_onesc(self):
        hook = _load_hook_with_py2_stubs()
        events = []

        class FakeCmdSvc:
            combatFunctionLoaded = None

            def OnEsc(self):
                events.append("OnEsc")

            def OpenFitting(self):
                events.append("OpenFitting")

        def record_close_menus(node):
            events.append(("close_menus", node))

        def record_dismiss_tooltip(node):
            events.append(("dismiss_tooltip", node))

        fake_uicore = types.SimpleNamespace(cmd=FakeCmdSvc(), registry=None, layer=None)
        fake_sm = mock.MagicMock()

        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_close_context_menus_if_outside", side_effect=record_close_menus), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", side_effect=record_dismiss_tooltip):
            res_esc = hook._execute_action(
                fake_sm, {"type": "press_key", "target": "", "command": "ESCAPE", "wait_ms": 0}
            )
            res_open = hook._execute_action(
                fake_sm, {"type": "press_key", "target": "ALT+F", "command": "", "wait_ms": 0}
            )

        self.assertTrue(res_esc.get("ok"), res_esc)
        self.assertTrue(res_open.get("ok"), res_open)
        self.assertEqual(
            events,
            [
                ("dismiss_tooltip", None),
                "OnEsc",
                ("dismiss_tooltip", None),
                ("close_menus", None),
                ("dismiss_tooltip", None),
                "OpenFitting",
            ],
        )


class TestHookSlashSpawnBallparkPoll(unittest.TestCase):
    def test_slash_spawn_polls_until_spawned_id_in_ballpark(self):
        hook = _load_hook_with_py2_stubs()
        polls = [0]
        balls = {}

        class FakeBallpark:
            pass

        bp = FakeBallpark()
        bp.balls = balls

        class FakeMichelle:
            def GetBallpark(self):
                polls[0] += 1
                if polls[0] >= 3:
                    balls[200198407] = object()
                return bp

        fake_sm = mock.MagicMock()
        fake_sm.GetService.side_effect = lambda name: FakeMichelle() if name == "michelle" else None
        sleep_calls = []

        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_run_gm_slash", return_value={"spawnedID": 200198407}), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
             mock.patch.object(hook.H, "_sleep_ms", create=True, side_effect=lambda ms: sleep_calls.append(ms)), \
             mock.patch.object(hook, "_get_uicore", return_value=types.SimpleNamespace(registry=None, layer=None)), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"):
            res = hook._execute_action(
                fake_sm, {"type": "slash_command", "command": "/spawn 587", "wait_ms": 0}
            )

        self.assertTrue(res.get("ok"), res)
        self.assertEqual(res.get("spawned_id"), 200198407)
        self.assertEqual(polls[0], 3)
        self.assertEqual(sleep_calls, [25, 100, 100])

    def test_jump_gate_tasklet_timeout_allows_over_12s(self):
        hook = _load_hook_with_py2_stubs()
        fake_sm = mock.MagicMock()
        fake_beyonce = mock.MagicMock()
        fake_mich = mock.MagicMock()
        fake_bp = mock.MagicMock()
        fake_bp.GetBall.return_value = types.SimpleNamespace(surfaceDist=1400.0)
        fake_bp.GetSurfaceDist.return_value = 1400.0
        fake_mich.GetBallpark.return_value = fake_bp
        fake_sm.GetService.side_effect = (
            lambda name: fake_beyonce
            if name == "beyonce"
            else (fake_mich if name == "michelle" else mock.MagicMock())
        )
        builtin_mod = hook.__builtin__
        orig_sess = getattr(builtin_mod, "session", None)
        setattr(
            builtin_mod,
            "session",
            types.SimpleNamespace(
                shipid=200197688,
                solarsystemid=30000144,
                solarsystemid2=30000144,
                stationid=None,
                structureid=None,
                mutating=0,
                changing=None,
            ),
        )
        t_clock = [1000.0]

        def fake_time():
            t_clock[0] += 0.5
            return t_clock[0]

        def _on_jump(gid):
            t_clock[0] += 13.5
            setattr(
                builtin_mod,
                "session",
                types.SimpleNamespace(
                    shipid=200197688,
                    solarsystemid=30000139,
                    solarsystemid2=30000139,
                    stationid=None,
                    structureid=None,
                    mutating=0,
                    changing=None,
                ),
            )

        fake_beyonce.CmdStargateJump.side_effect = _on_jump

        class FakeUthread:
            @staticmethod
            def new(fn):
                fn()

        try:
            with mock.patch.dict(sys.modules, {"uthread": FakeUthread}), \
                 mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                 mock.patch.object(hook, "_install_warp_telemetry"), \
                 mock.patch.object(hook.H, "_get_beyonce", create=True, return_value=fake_beyonce), \
                 mock.patch.object(hook.H, "_sleep_sim_ms", create=True), \
                 mock.patch.object(hook.H, "_sleep_ms", create=True), \
                 mock.patch.object(hook.H, "_wait_view_settled", create=True), \
                 mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
                 mock.patch.object(hook, "_get_uicore", return_value=types.SimpleNamespace(registry=None, layer=None)), \
                 mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
                 mock.patch.object(hook, "_close_utilmenu_if_outside"), \
                 mock.patch.object(hook, "_close_context_menus_if_outside"), \
                 mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
                 mock.patch.object(hook.time, "time", side_effect=fake_time):
                res = hook._execute_action(
                    fake_sm, {"type": "jump_gate", "target": "50002184", "wait_ms": 0}
                )
        finally:
            if orig_sess is None and hasattr(builtin_mod, "session"):
                delattr(builtin_mod, "session")
            else:
                setattr(builtin_mod, "session", orig_sess)

        self.assertTrue(res.get("ok"), res)
        self.assertEqual(res.get("from_system_id"), 30000144)
        self.assertEqual(res.get("to_system_id"), 30000139)

    def test_set_uilib_mouseover_handles_readonly_property_and_evomu_site_ignores_sev1_logtraceback(self):
        hook = _load_hook_with_py2_stubs()

        class ReadOnlyUilib(object):
            def __init__(self):
                self._mouseOver = None

            @property
            def mouseOver(self):
                return self._mouseOver() if callable(self._mouseOver) else self._mouseOver

        class WeakNode(object):
            pass

        uilib_ro = ReadOnlyUilib()
        node = WeakNode()
        hook._set_uilib_mouseover(uilib_ro, node)
        self.assertTrue(callable(uilib_ro._mouseOver))
        self.assertIs(uilib_ro.mouseOver, node)

        class RaisingSetterUilib(object):
            @property
            def mouseOver(self):
                return None

            @mouseOver.setter
            def mouseOver(self, _val):
                raise AttributeError("can't set attribute")

        with mock.patch.object(hook.sys, "exc_clear", create=True) as mock_clear:
            hook._set_uilib_mouseover(RaisingSetterUilib(), node)
        mock_clear.assert_called_once()

        with mock.patch.dict(sys.modules, {"__builtin__": builtins}):
            import evomu_site

            recorded = []
            target = types.SimpleNamespace(
                LogException=lambda *a, **kw: ("exc", a, kw),
                _LogException=lambda *a, **kw: ("_exc", a, kw),
                LogTraceback=lambda *a, **kw: ("tb", a, kw),
            )
            with mock.patch.object(evomu_site, "_log_exc_line", side_effect=recorded.append):
                evomu_site._wrap_log_target(target, "logmodule")
                try:
                    raise AttributeError("stale")
                except AttributeError:
                    out = target.LogTraceback("Localization module: No label", channel="Localization", severity=1)
                    out_warn = target.LogTraceback("UpdateItem - QUANTITY MISSING", channel="svc.godma", severity=2)
        self.assertEqual(out[0], "tb")
        self.assertEqual(out_warn[0], "tb")
        self.assertEqual(len(recorded), 1)
        self.assertIn("UpdateItem - QUANTITY MISSING", recorded[0])
        self.assertNotIn("AttributeError: stale", recorded[0])

    def test_close_window_waits_for_async_close_by_user_and_exits_ship_skinr(self):
        hook = _load_hook_with_py2_stubs()
        base_close_calls = []
        closed_views = []

        class FakeBaseWindow(object):
            @staticmethod
            def Close(wnd):
                base_close_calls.append(wnd)
                wnd.destroyed = True

        class AsyncDockablePanel(object):
            def __init__(self):
                self.windowID = "ShipSKINRWindow"
                self.panelID = "ShipSKINRWindow"
                self.name = "ShipSKINRWindow"
                self.caption = "Ship SKINR"
                self.destroyed = False
                self._pending_close = False

            def CloseByUser(self):
                self._pending_close = True

            def finish_async_close(self):
                if self._pending_close and not self.destroyed:
                    self.destroyed = True
                    closed_views.append("ship_skinr")

        wnd = AsyncDockablePanel()
        fake_dpm = mock.Mock()
        fake_vs = types.SimpleNamespace(
            GetCurrentViewInfo=lambda: types.SimpleNamespace(
                name="inflight" if "ship_skinr" in closed_views else "ship_skinr"
            ),
            CloseSecondaryView=lambda name: closed_views.append(name),
        )
        fake_sm = mock.Mock()
        fake_sm.GetService.side_effect = lambda name: fake_vs if name == "viewState" else mock.Mock()
        fake_uicore = types.SimpleNamespace(
            registry=types.SimpleNamespace(GetWindows=lambda: [wnd]),
            layer=None,
            dockablePanelManager=fake_dpm,
        )

        sleep_calls = []

        def _fake_sleep(ms):
            sleep_calls.append(ms)
            if len(sleep_calls) >= 2:
                wnd.finish_async_close()

        fake_window_mod = types.ModuleType("carbonui.control.window")
        fake_window_mod.Window = FakeBaseWindow

        with mock.patch.dict(sys.modules, {"carbonui.control.window": fake_window_mod}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_is_visible", return_value=True), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook.H, "_sleep_ms", create=True, side_effect=_fake_sleep), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            res = hook._execute_action(fake_sm, {"type": "close_window", "target": "ShipSKINRWindow", "wait_ms": 0})

        self.assertTrue(res.get("ok"), res)
        self.assertEqual(base_close_calls, [])
        self.assertTrue(wnd.destroyed)
        self.assertEqual(closed_views, ["ship_skinr"])

    def test_type_text_handles_non_cp1252_unicode_and_command_newline_submit(self):
        hook = _load_hook_with_py2_stubs()
        recorded = {"values": [], "submitted": []}

        class FakeEditField(object):
            name = "searchEdit"

            def SetValue(self, val):
                recorded["values"].append(val)

            def OnReturn(self):
                recorded["submitted"].append("OnReturn")

        edit_node = FakeEditField()
        fake_sm = mock.Mock()
        fake_uicore = types.SimpleNamespace(registry=types.SimpleNamespace(SetFocus=lambda _n: None), layer=None)
        payload = "Périmètre Периметр 日本 ☃🚀\n"

        with mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_find_widget", return_value=edit_node), \
             mock.patch.object(hook, "_resolve_editable_widget", return_value=edit_node), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            res = hook._execute_action(
                fake_sm,
                {"type": "type_text", "target": "searchEdit", "command": payload, "wait_ms": 0},
            )

        self.assertTrue(res.get("ok"), res)
        self.assertEqual(res.get("typed_into"), "searchEdit")
        self.assertEqual(res.get("submitted"), "OnReturn")
        self.assertEqual(recorded["values"], ["Périmètre Периметр 日本 ☃🚀"])
        self.assertEqual(recorded["submitted"], ["OnReturn"])

        class _FakePy2Unicode(object):
            def encode(self, enc, errors="strict"):
                return b"cyril" if enc == "utf-8" else b"?"

            def __str__(self):
                raise UnicodeEncodeError("charmap", "x", 0, 1, "not cp1252")

        with mock.patch.object(hook.sys, "version_info", (2, 7, 18, "final", 0)):
            orig_u = hook.__dict__.get("unicode")
            hook.__dict__["unicode"] = _FakePy2Unicode
            try:
                fake_u = _FakePy2Unicode()
                self.assertEqual(hook._to_utf8(fake_u), b"cyril")
                self.assertIs(hook._to_unicode(fake_u), fake_u)
            finally:
                if orig_u is not None:
                    hook.__dict__["unicode"] = orig_u

    def test_type_text_numeric_setvalue_rejection_returns_error_without_exception(self):
        hook = _load_hook_with_py2_stubs()

        class FakeIntEditField(object):
            name = "kmInput"

            def SetValue(self, val):
                if int(val) > 2147483647:
                    raise ValueError("value exceeds max int")

        edit_node = FakeIntEditField()
        fake_sm = mock.Mock()
        fake_uicore = types.SimpleNamespace(registry=types.SimpleNamespace(SetFocus=lambda _n: None), layer=None)

        with mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_find_widget", return_value=edit_node), \
             mock.patch.object(hook, "_resolve_editable_widget", return_value=edit_node), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            res = hook._execute_action(
                fake_sm,
                {"type": "type_text", "target": "kmInput", "command": "999999999999\\n", "wait_ms": 0},
            )

        self.assertFalse(res.get("ok"), res)
        self.assertIn("rejected", res.get("error", ""))
        self.assertEqual((res.get("flight_recorder") or {}).get("log_exceptions") or [], [])

    def test_wait_action_honors_multi_second_duration_and_reports_actual_waited_ms(self):
        hook = _load_hook_with_py2_stubs()
        fake_sm = mock.Mock()
        fake_uicore = types.SimpleNamespace(layer=None)
        slept = []

        with mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook.H, "_sleep_ms", create=True, side_effect=lambda ms: slept.append(ms)), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            res = hook._execute_action(
                fake_sm,
                {"type": "wait", "wait_ms": 12000, "strict_wait": True},
            )
            res_capped = hook._execute_action(
                fake_sm,
                {"type": "wait", "wait_ms": 45000, "strict_wait": True},
            )
            res_non_wait = hook._execute_action(
                fake_sm,
                {"type": "slash_command", "command": "/heal", "wait_ms": 12000},
            )

        self.assertEqual(res.get("waited_ms"), 12000)
        self.assertEqual(res_capped.get("waited_ms"), 30000)
        self.assertIn(12000, slept)
        self.assertIn(30000, slept)
        # Non-wait actions stay capped at 5000ms post-action settle delay.
        self.assertIn(5000, slept)
        self.assertNotIn(12000, slept[slept.index(30000) + 1:])
        self.assertTrue(res_non_wait.get("ok"))

    def test_hook_dispatch_cleanup_utility_windows_uses_bounded_uthread(self):
        hook = _load_hook_with_py2_stubs()
        srv = object.__new__(hook.PlaytesterRpcServer)
        srv.sm = mock.Mock()
        srv.cfg = {}

        fake_uthread = types.SimpleNamespace(new=lambda fn: fn())
        with mock.patch.dict(sys.modules, {"uthread": fake_uthread}), \
             mock.patch.object(
                 hook,
                 "_cleanup_utility_windows",
                 return_value={"ok": True, "closed_windows": ["JobBoardWindow"], "closed_secondary_view": None, "kept_windows": []},
             ) as cln_mock:
            res = srv._dispatch({"method": "cleanup_utility_windows", "params": {"keep_chat": True}})
        cln_mock.assert_called_once_with(srv.sm, keep_chat=True)
        self.assertTrue(res.get("ok"))
        self.assertEqual(res["cleanup_result"]["closed_windows"], ["JobBoardWindow"])

    def test_execute_action_captures_interstep_async_exceptions_and_err_flights(self):
        hook = _load_hook_with_py2_stubs()
        fake_es = types.SimpleNamespace(
            _EXC_SEQ=1,
            _RECENT_EXCEPTIONS=[
                {"seq": 1, "ts": 1.0, "headline": "KeyError: boot noise", "trace": 'File "boot.py", line 1'},
            ],
            _PT_CONSUMED_EXC_SEQ=1,
            _REPORTED_LOG_EXC_SIGS=set(),
        )
        fake_sm = mock.Mock()
        fake_sm.IsServiceRunning.return_value = False

        # Simulate an async traceback + macho error arriving while LLM thinks BETWEEN steps
        fake_es._EXC_SEQ = 3
        fake_es._RECENT_EXCEPTIONS.extend([
            {
                "seq": 2,
                "ts": 2.0,
                "headline": "KeyError: ('RecordNotFound', 500001)",
                "trace": 'File "targetInBar.py", line 370, in SetTargetLabel\nKeyError: (\'RecordNotFound\', 500001)',
            },
            {
                "seq": 3,
                "ts": 2.1,
                "headline": "KeyError: ('RecordNotFound', 500001)",
                "trace": 'File "targetInBar.py", line 370, in SetTargetLabel\nKeyError: (\'RecordNotFound\', 500001)',
            },
        ])
        hook._record_flight("macho", "dogmaIM", "AddTarget", "(200223470)", "ERR", 5.0, "Bang")

        with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(
                 hook,
                 "_get_state_snapshot",
                 return_value={
                     "open_windows": [],
                     "modals": [],
                     "ui": {},
                     "space": {},
                     "skills_state": {},
                     "wallet_state": {},
                     "active_view": "inflight",
                 },
             ), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook.H, "_sleep_sim_ms", create=True):
            res = hook._execute_action(fake_sm, {"type": "wait", "wait_ms": 0})
            fr = res["flight_recorder"]
            self.assertEqual(len(fr["log_exceptions"]), 1)
            self.assertIn("500001", fr["log_exceptions"][0]["headline"])
            self.assertEqual(len(fr["macho_exceptions"]), 1)
            self.assertEqual(fake_es._PT_CONSUMED_EXC_SEQ, 3)

            # Second execute_action should not replay already-consumed inter-step exceptions/errors
            res2 = hook._execute_action(fake_sm, {"type": "wait", "wait_ms": 0})
            self.assertEqual(res2["flight_recorder"]["log_exceptions"], [])
            self.assertEqual(res2["flight_recorder"]["macho_exceptions"], [])

    def test_lock_target_does_not_set_active_target_before_lock_completes(self):
        import builtins as builtin_mod

        hook = _load_hook_with_py2_stubs()
        st_svc = mock.Mock()
        tgt_svc = mock.Mock()
        tgt_svc.targetsByID = {}
        tgt_svc.GetTargeting.return_value = []
        tgt_svc.GetBallparkRecord.return_value = types.SimpleNamespace(groupID=25)
        fake_michelle = mock.Mock()
        fake_michelle.GetBallpark.return_value = None
        fake_sm = mock.Mock()
        fake_sm.GetService.side_effect = lambda name: {
            "target": tgt_svc,
            "stateSvc": st_svc,
            "michelle": fake_michelle,
            "viewState": None,
        }[name]

        fake_states_pkg = types.ModuleType("eve.client.script.parklife")
        fake_states = types.SimpleNamespace(selected=10, activeTarget=20)
        fake_states_pkg.states = fake_states
        fake_const = types.SimpleNamespace(nonTargetableGroups=())

        orig_sess = getattr(builtin_mod, "session", None)
        setattr(
            builtin_mod,
            "session",
            types.SimpleNamespace(
                shipid=200197688,
                solarsystemid=30000144,
                solarsystemid2=30000144,
                stationid=None,
                structureid=None,
            ),
        )
        try:
            with mock.patch.dict(sys.modules, {
                "eve": types.ModuleType("eve"),
                "eve.client": types.ModuleType("eve.client"),
                "eve.client.script": types.ModuleType("eve.client.script"),
                "eve.client.script.parklife": fake_states_pkg,
                "eve.client.script.parklife.states": fake_states,
                "const": fake_const,
            }), \
                 mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                 mock.patch.object(hook, "_install_warp_telemetry"), \
                 mock.patch.object(hook.H, "_get_beyonce", create=True, return_value=mock.Mock()), \
                 mock.patch.object(hook.H, "_sleep_ms", create=True), \
                 mock.patch.object(hook.H, "_sleep_sim_ms", create=True), \
                 mock.patch.object(
                     hook,
                     "_get_state_snapshot",
                     return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"},
                 ):
                res = hook._execute_action(fake_sm, {"type": "lock_target", "target": "200223704", "wait_ms": 0})
                self.assertTrue(res["ok"], res)
                st_svc.SetState.assert_called_once_with(200223704, 10, 1)
                tgt_svc.TryLockTarget.assert_called_once_with(200223704)

                # Once target is in targetsByID, calling lock_target marks it activeTarget too.
                st_svc.SetState.reset_mock()
                tgt_svc.targetsByID = {200223704: object()}
                res2 = hook._execute_action(fake_sm, {"type": "lock_target", "target": "200223704", "wait_ms": 0})
                self.assertTrue(res2["ok"], res2)
                self.assertEqual(
                    st_svc.SetState.call_args_list,
                    [mock.call(200223704, 10, 1), mock.call(200223704, 20, 1)],
                )
        finally:
            if orig_sess is None and hasattr(builtin_mod, "session"):
                delattr(builtin_mod, "session")
            elif orig_sess is not None:
                setattr(builtin_mod, "session", orig_sess)


class TestHookDockAndFlightRecorderAndEncoding(unittest.TestCase):
    def test_dock_space_command_adaptive_advance_clock_and_deadline(self):
        hook = _load_hook_with_py2_stubs()
        advance_calls = []
        dock_calls = []
        follow_calls = []
        dist_state = {"sd": 48000.0}
        fake_sess = types.SimpleNamespace(
            solarsystemid=30000144,
            stationid=None,
            structureid=None,
            shipid=200197688,
            mutating=0,
            changing=None,
        )

        class FakeTimeControl:
            def ClearSessionTimer(self):
                return None

            def AdvanceClock(self, secs):
                advance_calls.append(int(secs))
                dist_state["sd"] = max(0.0, dist_state["sd"] - float(secs) * 1000.0)
                if dist_state["sd"] <= 2000.0:
                    fake_sess.stationid = 60000685

        class FakeStationSvc:
            def Dock(self, dock_id):
                dock_calls.append(int(dock_id))
                if dist_state["sd"] > 2500.0:
                    raise RuntimeError("OutsideDockingPerimeter: still far")
                fake_sess.stationid = int(dock_id)

        class FakeBallpark:
            slimItems = {60000685: types.SimpleNamespace(categoryID=3)}

            def GetBall(self, bid):
                if bid == 60000685:
                    # Stale raw surfaceDist=0.0 must be superseded by GetSurfaceDist
                    return types.SimpleNamespace(mode=11, surfaceDist=0.0)
                if bid == 200197688:
                    return types.SimpleNamespace(mode=0)
                return None

            def GetSurfaceDist(self, a, b):
                return dist_state["sd"]

        fake_beyonce = types.SimpleNamespace(
            CmdFollowBall=lambda target, rng: follow_calls.append((int(target), float(rng)))
        )
        fake_bp = FakeBallpark()
        fake_michelle = types.SimpleNamespace(GetBallpark=lambda: fake_bp)
        fake_sm = mock.Mock()
        fake_sm.IsServiceRunning.return_value = False
        fake_sm.services = {"michelle": fake_michelle}
        fake_sm.GetService.side_effect = lambda name: {
            "michelle": fake_michelle,
            "viewState": types.SimpleNamespace(secondaryInfo=None),
        }.get(name)
        fake_sm.RemoteSvc.side_effect = lambda name: {
            "timeControl": FakeTimeControl(),
            "stationSvc": FakeStationSvc(),
        }[name]
        fake_es = types.SimpleNamespace(_EXC_SEQ=0, _RECENT_EXCEPTIONS=[], _PT_CONSUMED_EXC_SEQ=0, _REPORTED_LOG_EXC_SIGS=set())

        with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
             mock.patch.object(builtins, "session", fake_sess, create=True), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_get_beyonce", return_value=fake_beyonce, create=True), \
             mock.patch.object(hook.H, "_wait_view_settled", create=True), \
             mock.patch.object(hook.H, "_ensure_view", create=True), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook.H, "_sleep_sim_ms", create=True):
            res = hook._execute_action(fake_sm, {"type": "dock", "target": "60000685"})

        self.assertTrue(res.get("ok"), res)
        self.assertEqual(dock_calls, [60000685])
        self.assertEqual(follow_calls, [(60000685, 0.0)])
        self.assertEqual(advance_calls, [15, 15, 15, 8])

        # Second check: wall-clock budget bounds AdvanceClock iterations below tasklet timeout.
        advance_calls.clear()
        dock_calls.clear()
        dist_state["sd"] = 35000.0
        fake_sess.stationid = None
        fake_t = {"now": 1000.0}

        def _advancing_time():
            fake_t["now"] += 7.0
            return fake_t["now"]

        with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
             mock.patch.object(builtins, "session", fake_sess, create=True), \
             mock.patch.object(hook.time, "time", side_effect=_advancing_time), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
             mock.patch.object(hook.H, "_get_beyonce", return_value=fake_beyonce, create=True), \
             mock.patch.object(hook.H, "_wait_view_settled", create=True), \
             mock.patch.object(hook.H, "_ensure_view", create=True), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook.H, "_sleep_sim_ms", create=True):
            res_bounded = hook._execute_action(fake_sm, {"type": "dock", "target": "60000685"})

        self.assertFalse(res_bounded.get("ok"))
        self.assertIn("OutsideDockingPerimeter", res_bounded.get("error", ""))
        self.assertLessEqual(len(advance_calls), 3)

    def test_dock_retries_with_clock_advance_when_surface_dist_is_stale_zero(self):
        hook = _load_hook_with_py2_stubs()
        advance_calls = []
        dock_calls = []
        follow_calls = []
        fake_sess = types.SimpleNamespace(
            stationid=None,
            structureid=None,
            solarsystemid2=30000142,
            solarsystemid=30000142,
            shipid=200197688,
            mutating=0,
            changing=None,
        )

        class FakeTimeControl:
            def ClearSessionTimer(self):
                return None

            def AdvanceClock(self, secs):
                advance_calls.append(int(secs))

        class FakeStationSvc:
            def Dock(self, dock_id):
                dock_calls.append(int(dock_id))
                if len(advance_calls) < 2:
                    raise RuntimeError("UserError('DockingApproach', {})")
                fake_sess.stationid = int(dock_id)

        class FakeBallpark:
            slimItems = {60000403: types.SimpleNamespace(categoryID=3)}

            def GetBall(self, bid):
                if bid == 60000403:
                    return types.SimpleNamespace(mode=11, surfaceDist=0.0)
                if bid == 200197688:
                    return types.SimpleNamespace(mode=0)
                return None

            def GetSurfaceDist(self, a, b):
                return 0.0

        fake_beyonce = types.SimpleNamespace(
            CmdFollowBall=lambda target, rng: follow_calls.append((int(target), float(rng)))
        )
        fake_bp = FakeBallpark()
        fake_michelle = types.SimpleNamespace(GetBallpark=lambda: fake_bp)
        fake_sm = mock.Mock()
        fake_sm.IsServiceRunning.return_value = False
        fake_sm.services = {"michelle": fake_michelle}
        fake_sm.GetService.side_effect = lambda name: {
            "michelle": fake_michelle,
            "viewState": types.SimpleNamespace(secondaryInfo=None),
        }.get(name)
        fake_sm.RemoteSvc.side_effect = lambda name: {
            "timeControl": FakeTimeControl(),
            "stationSvc": FakeStationSvc(),
        }[name]
        fake_es = types.SimpleNamespace(_EXC_SEQ=0, _RECENT_EXCEPTIONS=[], _PT_CONSUMED_EXC_SEQ=0, _REPORTED_LOG_EXC_SIGS=set())

        with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
             mock.patch.object(builtins, "session", fake_sess, create=True), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_get_beyonce", return_value=fake_beyonce, create=True), \
             mock.patch.object(hook.H, "_wait_view_settled", create=True), \
             mock.patch.object(hook.H, "_ensure_view", create=True), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook.H, "_sleep_sim_ms", create=True):
            res = hook._execute_action(fake_sm, {"type": "dock", "target": "60000403"})

        self.assertTrue(res.get("ok"), res)
        self.assertGreaterEqual(len(advance_calls), 2)
        self.assertGreaterEqual(len(dock_calls), 3)
        self.assertEqual(follow_calls, [(60000403, 0.0)])

    def test_flight_recorder_captures_cached_core_service_call_without_double_recording(self):
        import types
        import weakref

        hook = _load_hook_with_py2_stubs()
        carbon_mod = types.ModuleType("carbon")
        carbon_common = types.ModuleType("carbon.common")
        carbon_script = types.ModuleType("carbon.common.script")
        carbon_net = types.ModuleType("carbon.common.script.net")
        sc_gpcs_mod = types.ModuleType("carbon.common.script.net.ServiceCallGPCS")
        mon_mod = types.ModuleType("carbon.common.script.net.moniker")
        oc_mod = types.ModuleType("carbon.common.script.net.ObjectCallGPCS")

        class FakeCoreServiceCall(object):
            def __init__(self):
                self.calls = []

            def RemoteServiceCallWithoutTheStars(self, sess, nodeID, service, method, args=(), keywords=None):
                self.calls.append((service, method, args, keywords))
                if method == "RaiseErr":
                    raise RuntimeError("boom")
                return "ok:" + str(method)

        class FakeServiceCallSubclass(FakeCoreServiceCall):
            pass

        class FakeCachedProxy(object):
            def __init__(self, gpcs, svc):
                self.gpcs = weakref.proxy(gpcs)
                self.svc = svc

            def CreateContract(self, *args, **kwargs):
                return self.gpcs.RemoteServiceCallWithoutTheStars(None, None, self.svc, "CreateContract", args, kwargs)

            def RaiseErr(self, *args, **kwargs):
                return self.gpcs.RemoteServiceCallWithoutTheStars(None, None, self.svc, "RaiseErr", args, kwargs)

        sc_gpcs_mod.CoreServiceCall = FakeCoreServiceCall
        carbon_net.ServiceCallGPCS = sc_gpcs_mod
        carbon_net.moniker = mon_mod
        carbon_net.ObjectCallGPCS = oc_mod

        fake_mods = {
            "carbon": carbon_mod,
            "carbon.common": carbon_common,
            "carbon.common.script": carbon_script,
            "carbon.common.script.net": carbon_net,
            "carbon.common.script.net.ServiceCallGPCS": sc_gpcs_mod,
            "carbon.common.script.net.moniker": mon_mod,
            "carbon.common.script.net.ObjectCallGPCS": oc_mod,
        }

        gpcs = FakeServiceCallSubclass()
        cached_proxy = FakeCachedProxy(gpcs, "contractProxy")

        def raw_remote(name):
            return FakeCachedProxy(gpcs, name)

        # Simulate stale pre-generation wrapper (V1) whose closure captured orig_remote
        orig_remote = raw_remote

        def legacy_v1_remote(svc_name, *args, **kwargs):
            return orig_remote(svc_name)

        legacy_v1_remote._evomu_pt_wrapped = True
        fake_sm = types.SimpleNamespace(RemoteSvc=legacy_v1_remote, ProxySvc=raw_remote)

        with mock.patch.dict(sys.modules, fake_mods), \
             mock.patch.object(hook, "_HOOKED_REMOTESVC", False), \
             mock.patch.object(hook, "_HOOK_RELOAD_GEN", 1), \
             mock.patch.object(hook, "_FLIGHT_RECORDER", []), \
             mock.patch.object(hook, "_MACHO_CALL_TASKLET_DEPTH", {}):
            hook._install_flight_recorder_hooks(fake_sm)

            # Legacy V1 wrapper must have been unwrapped back to raw_remote cleanly
            self.assertIs(fake_sm.RemoteSvc._evomu_pt_orig, raw_remote)
            self.assertEqual(fake_sm.RemoteSvc._evomu_pt_gen, 1)

            # 1) Pre-cached boot proxy via weakref.proxy(ServiceCall) records OK and ERR
            self.assertEqual(cached_proxy.CreateContract(1, 2), "ok:CreateContract")
            with self.assertRaises(RuntimeError):
                cached_proxy.RaiseErr("bad")

            # 2) Post-hook sm.ProxySvc & sm.RemoteSvc calls record ONCE each (not doubled!)
            fresh_proxy = fake_sm.ProxySvc("contractProxy")
            self.assertEqual(fresh_proxy.CreateContract("fresh"), "ok:CreateContract")
            remote_proxy = fake_sm.RemoteSvc("account")
            self.assertEqual(remote_proxy.CreateContract("acct"), "ok:CreateContract")

            recorded = list(hook._FLIGHT_RECORDER)
            self.assertEqual(
                [(r["service"], r["method"], r["status"]) for r in recorded],
                [
                    ("contractProxy", "CreateContract", "OK"),
                    ("contractProxy", "RaiseErr", "ERR"),
                    ("contractProxy", "CreateContract", "OK"),
                    ("account", "CreateContract", "OK"),
                ],
            )
            self.assertEqual(len(gpcs.calls), 4)

    def test_settle_secondary_view_transition_waits_for_skinr_page_container_then_stabilizes(self):
        import types

        hook = _load_hook_with_py2_stubs()

        class FakeViewState:
            def __init__(self, view_name):
                self._view_name = view_name

            def GetCurrentViewInfo(self):
                return types.SimpleNamespace(name=self._view_name)

        def make_sm(view_name):
            return types.SimpleNamespace(services={"viewState": FakeViewState(view_name)})

        # 1) Normal non-transition or non-async view does zero sleeping
        sleeps = []
        hook._settle_secondary_view_transition(
            make_sm("hangar"),
            {"active_view": "ship_skinr"},
            inspect_ui_fn=lambda: {"clickable_widgets": [], "modals": []},
            sleep_fn=sleeps.append,
        )
        hook._settle_secondary_view_transition(
            make_sm("ship_skinr"),
            {"active_view": "ship_skinr"},
            inspect_ui_fn=lambda: {"clickable_widgets": [], "modals": []},
            sleep_fn=sleeps.append,
        )
        self.assertEqual(sleeps, [])

        # 2) Hangar -> ship_skinr waits while only chrome shell (17 widgets, no page_container/)
        #    is mounted, then waits for 2 stable polls after page_container/ mounts (33 widgets).
        shell_widgets = [{"path": f"ShipSKINRWindow/header/b{i}"} for i in range(17)]
        loaded_widgets = shell_widgets + [
            {"path": "ShipSKINRWindow/content/main/page_container/studio_page/StudioHomePage/btn"}
            for _ in range(16)
        ]
        snapshots = iter(
            [
                {"clickable_widgets": shell_widgets, "modals": []},  # initial
                {"clickable_widgets": shell_widgets, "modals": []},  # poll 1: still blank shell
                {"clickable_widgets": loaded_widgets, "modals": []},  # poll 2: page mounted
                {"clickable_widgets": loaded_widgets, "modals": []},  # poll 3: stable 1
                {"clickable_widgets": loaded_widgets, "modals": []},  # poll 4: stable 2 -> exit
                {"clickable_widgets": loaded_widgets, "modals": []},
            ]
        )
        sleeps = []
        hook._settle_secondary_view_transition(
            make_sm("ship_skinr"),
            {"active_view": "hangar"},
            inspect_ui_fn=lambda: next(snapshots),
            sleep_fn=sleeps.append,
            max_polls=10,
            poll_ms=250,
        )
        self.assertEqual(sleeps, [250, 250, 250, 250])

        # 3) If permanently blank, caps at max_polls so genuine blank-window bugs are still caught
        sleeps = []
        hook._settle_secondary_view_transition(
            make_sm("ship_skinr"),
            {"active_view": "hangar"},
            inspect_ui_fn=lambda: {"clickable_widgets": shell_widgets, "modals": []},
            sleep_fn=sleeps.append,
            max_polls=5,
            poll_ms=250,
        )
        self.assertEqual(len(sleeps), 5)

        # 4) Stops immediately if a modal pops
        sleeps = []
        hook._settle_secondary_view_transition(
            make_sm("ship_skinr"),
            {"active_view": "hangar"},
            inspect_ui_fn=lambda: {"clickable_widgets": shell_widgets, "modals": [{"name": "modal"}]},
            sleep_fn=sleeps.append,
            max_polls=5,
            poll_ms=250,
        )
        self.assertEqual(sleeps, [])

    def test_utf8_truncation_and_json_safe_handle_multibyte_strings(self):
        hook = _load_hook_with_py2_stubs()
        payload = "Chaos Test Ω≈ç√∫ αβγδεζηθ 0123456789 !@#$%^&*()_+"
        # Repro condition: utf-8 encoded byte slice [:40] splits 2-byte 'θ' (0xce 0xb8) after 0xce
        raw_utf8 = payload.encode("utf-8")
        self.assertEqual(raw_utf8[39:40], b"\xce")

        # _safe_str, _utf8_trunc, and _strip_tags slice on characters, preserving valid UTF-8
        s_trunc = hook._safe_str(payload, 40)
        self.assertEqual(s_trunc, payload[:40])
        u_trunc = hook._utf8_trunc(payload, 40)
        self.assertEqual(u_trunc, payload[:40])
        stripped = hook._strip_tags(f"<b>{payload}</b>")
        self.assertEqual(stripped, payload)

        # Simulate Python 2.7 str/unicode path for _safe_str, _strip_tags, and _json_safe
        class _Py2Unicode(str):
            def encode(self, enc="utf-8", errors="replace"):
                return _Py2Bytes(super().encode(enc, errors))

            def split(self, *args, **kwargs):
                return [_Py2Unicode(p) for p in super().split(*args, **kwargs)]

            def join(self, parts):
                return _Py2Unicode(super().join(parts))

        class _Py2Bytes(bytes):
            def __str__(self):
                return self.decode("utf-8", "replace")

            def split(self, *args, **kwargs):
                return super().split(*args, **kwargs)

            def decode(self, enc="utf-8", errors="strict"):
                return _Py2Unicode(super().decode(enc, errors))

        with mock.patch.object(hook.sys, "version_info", (2, 7, 15, "final", 0)), \
             mock.patch.dict(hook.__dict__, {"unicode": _Py2Unicode, "long": int, "str": _Py2Bytes}):
            # Verify _safe_str slices at Unicode chars rather than mid-UTF-8 bytes
            safe_py2 = hook._safe_str(_Py2Unicode(payload), 40)
            self.assertIsInstance(safe_py2, bytes)
            self.assertEqual(safe_py2.decode("utf-8"), payload[:40])

            # Verify _json_safe converts malformed truncated UTF-8 bytes cleanly instead of failing json.dumps
            bad_bytes = _Py2Bytes(raw_utf8[:40])
            sanitized = hook._json_safe({_Py2Bytes(b"label"): bad_bytes, _Py2Unicode("nested"): [bad_bytes, 123, True]})
            self.assertEqual(sanitized["label"], "Chaos Test Ω≈ç√∫ αβγδεζη\ufffd")
            self.assertEqual(sanitized["nested"][1:], [123, True])
            encoded = json.dumps(sanitized)
            self.assertIn("\\u03a9", encoded)

            # Verify _strip_tags handles non-ASCII unicode safely under Py2 rules
            stripped_py2 = hook._strip_tags(_Py2Unicode(f"<i>{payload}</i>"))
            stripped_txt = stripped_py2.decode("utf-8") if isinstance(stripped_py2, bytes) else stripped_py2
            self.assertEqual(stripped_txt, payload)

    def test_evomu_site_log_exc_line_ignores_exception_end(self):
        eve_tb = (
            "EXCEPTION #3 logged at 10/11/26 19:51:08 : Could not fetch owned ship cosmetic licenses: \n\n"
            "TimeoutException\n\n"
            "Common path prefix = \n"
            "  File \"packages/shipcosmetics/client/licensegateway/quasar.py\", line 117, in _list_owned_licenses\n"
            "    return list_licenses_response.licenses\n\n"
            "EXCEPTION END\n"
        )
        site_path = HARNESS_DIR / "evomu_site.py"
        spec = importlib.util.spec_from_file_location("evomu_site_headline_test", site_path)
        site_mod = importlib.util.module_from_spec(spec)
        fake_builtin = types.ModuleType("__builtin__")
        with mock.patch.dict(sys.modules, {"__builtin__": fake_builtin}):
            spec.loader.exec_module(site_mod)
        site_mod._RECENT_EXCEPTIONS.clear()
        site_mod._log_exc_line(eve_tb)
        self.assertEqual(len(site_mod._RECENT_EXCEPTIONS), 1)
        self.assertEqual(site_mod._RECENT_EXCEPTIONS[0]["headline"], "TimeoutException")


class TestHookSlashCommandListSurface(unittest.TestCase):
    def test_run_gm_slash_root_forces_live_fetch_and_returns_command_list(self):
        hook = _load_hook_with_py2_stubs()

        class FakeSlashSvc:
            def __init__(self):
                self.remoteCommandList = ["stale_cached_only"]
                self.fetch_calls = 0

            def FetchRemoteCommandList(self):
                if hasattr(self, "remoteCommandList"):
                    return
                self.fetch_calls += 1
                self.remoteCommandList = ["advancetime", "createitem", "giveisk", "heal", "spawn"]

        class FakeSM:
            def __init__(self, slash_svc):
                self._svc = slash_svc

            def GetService(self, name):
                assert name == "slash"
                return self._svc

        svc = FakeSlashSvc()
        res = hook._run_gm_slash(FakeSM(svc), "/")
        self.assertEqual(svc.fetch_calls, 1)
        self.assertIn("Commands: [", res)
        self.assertIn("'createitem'", res)
        self.assertNotIn("stale_cached_only", res)


class TestEvomuSiteFsdPreloadHook(unittest.TestCase):
    def _load_fresh_evomu_site(self):
        import builtins
        import importlib.util
        site_path = HARNESS_DIR / "evomu_site.py"
        spec = importlib.util.spec_from_file_location("evomu_site_fsd_test", site_path)
        mod = importlib.util.module_from_spec(spec)
        with mock.patch.dict(sys.modules, {"__builtin__": builtins}):
            spec.loader.exec_module(mod)
        return mod

    def test_preload_hook_loads_v24_typelist_without_loadbinary_arg_error(self):
        evomu_site = self._load_fresh_evomu_site()

        bdl_mod = types.ModuleType("fsdBuiltData.common.base")
        res_enabled = {"val": True}

        def use_res_file_server():
            return res_enabled["val"]

        def disable_res_file_server():
            res_enabled["val"] = False

        def enable_res_file_server():
            res_enabled["val"] = True

        bdl_mod.UseResFileServer = use_res_file_server
        bdl_mod.DisableUseResFileServer = disable_res_file_server
        bdl_mod.EnableUseResFileServer = enable_res_file_server

        class BuiltDataLoader(object):
            __binary__ = None

            @classmethod
            def _BuiltDataLoader__LoadBinary(cls):
                # Simulates async ResFileServer returning None while UseResFileServer() is True,
                # and local staticdata loading cleanly once DisableUseResFileServer() is active.
                if res_enabled["val"]:
                    return None
                return {140: {"renderable": True}}

            @classmethod
            def GetData(cls):
                if cls.__binary__ is None:
                    cls.__binary__ = cls._BuiltDataLoader__LoadBinary()
                return cls.__binary__

        bdl_mod.BuiltDataLoader = BuiltDataLoader

        evetypes_data = types.ModuleType("evetypes.data")

        class FakeTypeListLoader(BuiltDataLoader):
            pass

        evetypes_data.TypeListLoader = FakeTypeListLoader

        with mock.patch.dict(
            sys.modules,
            {"fsdBuiltData.common.base": bdl_mod, "evetypes.data": evetypes_data},
        ):
            # Step 1: Under find_module import lock (preload_typelist=False), only GetData is patched.
            evomu_site._install_fsd_typelist_preload_hook(preload_typelist=False)
            self.assertTrue(getattr(BuiltDataLoader, "_evomu_getdata_patched", False))
            self.assertIsNone(FakeTypeListLoader.__binary__)
            self.assertFalse(evomu_site._HOOKED_TYPELIST_PRELOAD)

            # Step 2: Eager preload from runtime tasklet populates __binary__
            # and restores UseResFileServer.__enabled__.
            evomu_site._install_fsd_typelist_preload_hook(preload_typelist=True)
            self.assertEqual(
                FakeTypeListLoader.__binary__,
                {140: {"renderable": True}},
            )
            self.assertEqual(FakeTypeListLoader.GetData(), {140: {"renderable": True}})
            self.assertTrue(res_enabled["val"])
            self.assertTrue(evomu_site._HOOKED_TYPELIST_PRELOAD)

    def test_safe_getdata_falls_back_to_zero_arg_loadbinary_for_other_subclasses(self):
        evomu_site = self._load_fresh_evomu_site()

        bdl_mod = types.ModuleType("fsdBuiltData.common.base")
        calls = []

        class BuiltDataLoader(object):
            __binary__ = None

            @classmethod
            def _BuiltDataLoader__LoadBinary(cls):
                calls.append(cls.__name__)
                return {"ok": True}

            @classmethod
            def GetData(cls):
                # Simulate initial async None from upstream GetData.
                return None

        bdl_mod.BuiltDataLoader = BuiltDataLoader

        with mock.patch.dict(sys.modules, {"fsdBuiltData.common.base": bdl_mod}):
            evomu_site._install_fsd_typelist_preload_hook(preload_typelist=False)

            class OtherLoader(BuiltDataLoader):
                pass

            data = OtherLoader.GetData()
            self.assertEqual(data, {"ok": True})
            self.assertEqual(OtherLoader.__binary__, {"ok": True})
            self.assertEqual(calls, ["OtherLoader"])


class TestStdlogutilsTracebacksAndSuppressedSymptoms(unittest.TestCase):
    def test_stdlogutils_py_line_frames_prevent_keyerror_collision_and_surface_suppressed(self):
        hook = _load_hook_with_py2_stubs()
        tb1 = (
            "stdlogutils.EveExceptionsFormatter logger=root:\n"
            "EXCEPTION #7 : ...\n"
            "Formatted exception info:\nKeyError: 0\n"
            "Caught at:\n"
            "/eve/release/V24.01/packages/bluepy/__init__.py(55) CallWrapper\n"
            "Thrown at:\n"
            "/eve/release/V24.01/eve/client/script/ui/shared/neocom/journal.py(470) IsMissionActiveWithAgent\n"
            "/eve/release/V24.01/packages/utillib/__init__.py(37) __getitem__\n"
        )
        tb2 = (
            "stdlogutils.EveExceptionsFormatter logger=root:\n"
            "EXCEPTION #8 : ...\n"
            "Formatted exception info:\nKeyError: 0\n"
            "Thrown at:\n"
            "/eve/release/V24.01/eve/client/script/ui/shared/market/marketWindow.py(99) LoadOrders\n"
        )
        fake_es = types.SimpleNamespace(
            _EXC_SEQ=3,
            _RECENT_EXCEPTIONS=[
                {"seq": 1, "ts": 1.0, "headline": "KeyError: 0", "trace": tb1},
                {"seq": 2, "ts": 1.5, "headline": "KeyError: 0", "trace": tb1},
                {"seq": 3, "ts": 2.0, "headline": "KeyError: 0", "trace": tb2},
            ],
            _PT_CONSUMED_EXC_SEQ=0,
            _REPORTED_LOG_EXC_SIGS=set(),
        )
        fake_sm = mock.Mock()
        fake_sm.IsServiceRunning.return_value = False
        with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(
                 hook,
                 "_get_state_snapshot",
                 return_value={
                     "open_windows": [],
                     "modals": [],
                     "ui": {},
                     "space": {},
                     "skills_state": {},
                     "wallet_state": {},
                     "active_view": "hangar",
                 },
             ), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook.H, "_sleep_sim_ms", create=True):
            res = hook._execute_action(fake_sm, {"type": "wait", "wait_ms": 0})
        fr = res["flight_recorder"]
        # Both distinct KeyError: 0 stdlogutils sites must be preserved; duplicate tb1 goes to suppressed_log_exceptions
        self.assertEqual(len(fr["log_exceptions"]), 2)
        self.assertEqual(len(fr["suppressed_log_exceptions"]), 1)
        self.assertIn("journal.py(470)", fr["suppressed_log_exceptions"][0]["sig"])

        # Long tracebacks in evomu_site._log_exc_line keep head + tail past 1200 chars
        site_spec = importlib.util.spec_from_file_location("_evomu_site_test", str(HARNESS_DIR / "evomu_site.py"))
        site_mod = importlib.util.module_from_spec(site_spec)
        with mock.patch.dict(sys.modules, {"__builtin__": builtins}):
            site_spec.loader.exec_module(site_mod)
        long_msg = "EXCEPTION #99\nCaught at:\n" + ("frame_a\n" * 300) + "Thrown at:\njournal.py(470) IsMissionActiveWithAgent\nKeyError: 0\n"
        site_mod._RECENT_EXCEPTIONS.clear()
        with mock.patch.dict("os.environ", {"EVOMU_INCLIENT_CFG": ""}, clear=False):
            site_mod._log_exc_line(long_msg)
        self.assertIn("journal.py(470) IsMissionActiveWithAgent", site_mod._RECENT_EXCEPTIONS[-1]["trace"])


class TestHookInlineLabelUrlClicks(unittest.TestCase):
    def test_click_widget_primes_label_inline_link_url(self):
        hook = _load_hook_with_py2_stubs()

        class _InlineObj:
            def __init__(self, inline_type, data, x0, x1, y0=0, h=17):
                self.inlineType = inline_type
                self.data = data
                self.inlineX = x0
                self.inlineXEnd = x1
                self.inlineY = y0
                self.inlineHeight = h

        class _FakeUilib:
            def __init__(self):
                self.x = 0
                self.y = 0
                self.mouseOver = None

        class _FakeLabel:
            name = "openFwWindow"

            def __init__(self, uilib):
                self._uilib = uilib
                self._inlineObjects = [
                    _InlineObj("link", "openfw:", 120, 220, 0, 17),
                ]
                self._mouseOverUrl = None
                self.clicked_urls = []
                self.children = []

            def GetAbsolute(self):
                return (139, 398, 456, 17)

            def CheckInlines(self):
                if self._uilib.mouseOver is self:
                    lx, ly = 139, 398
                    for inl in self._inlineObjects:
                        if (
                            lx + inl.inlineX < self._uilib.x < lx + inl.inlineXEnd
                            and ly + inl.inlineY < self._uilib.y < ly + inl.inlineHeight
                            and inl.inlineType == "link"
                        ):
                            self._mouseOverUrl = inl.data
                            return
                self._mouseOverUrl = None

            def OnClick(self):
                if self._mouseOverUrl:
                    self.clicked_urls.append(self._mouseOverUrl)

        uilib = _FakeUilib()
        fake_uicore = types.SimpleNamespace(uilib=uilib, registry=None, cmd=None, layer=None)
        lbl = _FakeLabel(uilib)

        with mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), mock.patch.object(
            hook, "_close_neocom_panels_if_outside"
        ), mock.patch.object(hook, "_close_utilmenu_if_outside"), mock.patch.object(
            hook, "_close_context_menus_if_outside"
        ), mock.patch.object(
            hook, "_dismiss_pinned_tooltip_if_outside", return_value=False
        ):
            # Center click x=367 misses inline [139+120=259 .. 139+220=359] unless primed
            res = hook._invoke_widget_click(lbl, click_x=367, click_y=406)

        self.assertEqual(res, "OnClick")
        self.assertEqual(lbl.clicked_urls, ["openfw:"])
        self.assertTrue(259 < uilib.x < 359)


class TestHookModalReplacementDetection(unittest.TestCase):
    def test_execute_action_modal_replaces_existing_modal_does_not_timeout(self):
        hook = _load_hook_with_py2_stubs()

        class SkillRequirementDialog:
            windowID = "skill_requirement_dialog"
            name = "skill_requirement_dialog"
            isModal = True
            destroyed = False
            display = True

        class MissingSkillbooksWnd:
            windowID = "missingSkillbooksWnd"
            name = "missingSkillbooksWnd"
            isModal = True
            destroyed = False
            display = True

        active_windows = [SkillRequirementDialog()]
        fake_reg = types.SimpleNamespace(GetWindows=lambda: list(active_windows))
        fake_uicore = types.SimpleNamespace(registry=fake_reg, layer=None, cmd=None)
        fake_sm = mock.MagicMock()

        def fake_uthread_new(fn):
            # Simulate BuyAndTrainButton closing skill_requirement_dialog and opening
            # missingSkillbooksWnd synchronously inside ShowModal() before returning.
            active_windows[:] = [MissingSkillbooksWnd()]

        fake_uthread = types.SimpleNamespace(new=fake_uthread_new)
        clock = {"t": 1000.0}

        def fake_time():
            return clock["t"]

        def fake_sleep_ms(ms):
            clock["t"] += float(ms) / 1000.0

        with mock.patch.dict(sys.modules, {"uthread": fake_uthread}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook, "_is_visible", side_effect=lambda w: bool(getattr(w, "display", True))), \
             mock.patch.object(hook.H, "_sleep_ms", side_effect=fake_sleep_ms, create=True), \
             mock.patch.object(hook.time, "time", side_effect=fake_time), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"):
            res = hook._execute_action(
                fake_sm,
                {"type": "click_widget", "target": "BuyAndTrainButton", "wait_ms": 0},
            )

        self.assertTrue(res.get("ok"), res)
        self.assertTrue(res.get("blocked_by_modal"), res)
        self.assertIsNone(res.get("error"), res)


class TestHookClippedWidgetsAndTypeSubmit(unittest.TestCase):
    def test_find_widget_at_xy_and_inspect_skip_clipped_scroll_descendants(self):
        hook = _load_hook_with_py2_stubs()

        class FakeNode:
            def __init__(self, name, cls_name, x, y, w, h, parent=None, clip_children=False, click_handler=False, editable=False):
                self.name = name
                self._cls_name = cls_name
                self._rect = (x, y, w, h)
                self.parent = parent
                self.clipChildren = clip_children
                self.children = []
                self.display = True
                self.destroyed = False
                self.state = 0
                self._click_handler = click_handler
                if editable:
                    self.SetValue = lambda _v: None
                if parent is not None:
                    parent.children.append(self)

            def GetAbsolute(self):
                return self._rect

            def HasEventHandler(self, evt):
                return self._click_handler and evt == "OnClick"

        main_layer = FakeNode("l_main", "Layer", 0, 0, 1280, 800)
        contracts_wnd = FakeNode("contracts", "ContractsWindow", 320, 150, 700, 560, parent=main_layer)
        contracts_wnd.windowID = "contracts"
        left_cont = FakeNode("leftCont", "Container", 335, 220, 184, 480, parent=contracts_wnd)
        scroll = FakeNode("leftFilterScroll", "ScrollContainer", 335, 252, 184, 414, parent=left_cont)
        clip_cont = FakeNode("clipCont", "Container", 335, 252, 184, 414, parent=scroll, clip_children=True)
        visible_type = FakeNode("type", "SingleLineEditText", 335, 256, 168, 32, parent=clip_cont, click_handler=True, editable=True)
        # Scrolled outside clipCont (y=672..704 > clipCont bottom 666) and smaller area (5376 < 5888) behind searchBtn
        hidden_issuer = FakeNode("issuer", "SingleLineEditText", 335, 672, 168, 32, parent=clip_cont, click_handler=True, editable=True)
        search_btn = FakeNode("searchBtn", "Button", 335, 668, 184, 32, parent=left_cont, click_handler=True)
        search_btn.label = "Search"

        fake_layers = types.SimpleNamespace(main=main_layer)
        fake_reg = types.SimpleNamespace(GetWindows=lambda: [contracts_wnd])
        fake_uicore = types.SimpleNamespace(layer=fake_layers, desktop=None, registry=fake_reg)

        with mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            ui_tree = hook._inspect_uicore_tree()
            hit = hook._find_widget_at_xy(427, 686, fake_layers)

        names = [w["name"] for w in ui_tree["clickable_widgets"]]
        self.assertIn("type", names)
        self.assertIn("searchBtn", names)
        self.assertNotIn("issuer", names)
        self.assertIs(hit, search_btn)
        self.assertEqual(hook._widget_clipped_rect(visible_type), (335, 256, 168, 32))
        self.assertEqual(hook._widget_clipped_rect(hidden_issuer)[3], 0)

    def test_type_text_submit_does_not_swallow_handler_exception_or_double_submit(self):
        hook = _load_hook_with_py2_stubs()
        confirm_calls = []

        class BrokenOnReturnEdit(object):
            name = "type"

            def SetValue(self, val):
                self.val = val

            def _nested_client_search(self):
                raise AttributeError("KeyVal instance has no attribute 'itemTypeID'")

            def OnReturn(self):
                self._nested_client_search()

            def Confirm(self):
                confirm_calls.append("Confirm")

        edit_node = BrokenOnReturnEdit()
        fake_sm = mock.Mock()
        fake_uicore = types.SimpleNamespace(registry=types.SimpleNamespace(SetFocus=lambda _n: None), layer=None)

        with mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_find_widget", return_value=edit_node), \
             mock.patch.object(hook, "_resolve_editable_widget", return_value=edit_node), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            res = hook._execute_action(
                fake_sm,
                {"type": "type_text", "target": "type", "command": "Tritanium\n", "wait_ms": 0},
            )

        self.assertFalse(res.get("ok"), res)
        self.assertIn("AttributeError", res.get("error", ""))
        self.assertEqual(confirm_calls, [])

    def test_type_text_submit_arity_mismatch_falls_back_to_confirm(self):
        hook = _load_hook_with_py2_stubs()
        confirm_calls = []

        class ArityMismatchOnReturnEdit(object):
            name = "type"

            def SetValue(self, val):
                self.val = val

            def OnReturn(self, _unused_required_arg):
                raise AssertionError("should not execute")

            def Confirm(self):
                confirm_calls.append("Confirm")

        edit_node = ArityMismatchOnReturnEdit()
        fake_sm = mock.Mock()
        fake_uicore = types.SimpleNamespace(registry=types.SimpleNamespace(SetFocus=lambda _n: None), layer=None)

        with mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_find_widget", return_value=edit_node), \
             mock.patch.object(hook, "_resolve_editable_widget", return_value=edit_node), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            res = hook._execute_action(
                fake_sm,
                {"type": "type_text", "target": "type", "command": "Tritanium\n", "wait_ms": 0},
            )

        self.assertTrue(res.get("ok"), res)
        self.assertEqual(res.get("submitted"), "Confirm")
        self.assertEqual(confirm_calls, ["Confirm"])


class TestSpaceCommandTargetResolution(unittest.TestCase):
    def test_lock_target_resolves_named_ball_and_rejects_unmatched_target(self):
        import builtins as builtin_mod

        hook = _load_hook_with_py2_stubs()
        st_svc = mock.Mock()
        tgt_svc = mock.Mock()
        tgt_svc.targetsByID = {}
        tgt_svc.GetTargeting.return_value = []
        tgt_svc.GetBallparkRecord.side_effect = lambda tid: types.SimpleNamespace(groupID=485 if tid == 1897020878 else 28)

        ego_id = 1897012673
        badger_id = 801103271
        rev_id = 1897020878
        fake_bp = types.SimpleNamespace(
            slimItems={
                ego_id: types.SimpleNamespace(name="Avatar", typeID=11567, groupID=30, categoryID=6),
                badger_id: types.SimpleNamespace(name="Badger", typeID=648, groupID=28, categoryID=6),
                rev_id: types.SimpleNamespace(name="Revelation", typeID=19720, groupID=485, categoryID=6),
            },
            GetBall=lambda bid: types.SimpleNamespace(
                surfaceDist={ego_id: 0.0, badger_id: 61116.8, rev_id: 64035.6}[int(bid)],
                mode=2,
            ),
            GetSurfaceDist=lambda a, b: {badger_id: 61116.8, rev_id: 64035.6}[int(b)],
        )
        fake_michelle = mock.Mock()
        fake_michelle.GetBallpark.return_value = fake_bp
        fake_sm = mock.Mock()
        fake_sm.GetService.side_effect = lambda name: {
            "target": tgt_svc,
            "stateSvc": st_svc,
            "michelle": fake_michelle,
            "viewState": None,
        }[name]

        fake_states_pkg = types.ModuleType("eve.client.script.parklife")
        fake_states = types.SimpleNamespace(selected=10, activeTarget=20)
        fake_states_pkg.states = fake_states
        fake_const = types.SimpleNamespace(nonTargetableGroups=())

        orig_sess = getattr(builtin_mod, "session", None)
        setattr(
            builtin_mod,
            "session",
            types.SimpleNamespace(
                shipid=ego_id,
                solarsystemid=30003271,
                solarsystemid2=30003271,
                stationid=None,
                structureid=None,
            ),
        )
        try:
            with mock.patch.dict(sys.modules, {
                "eve": types.ModuleType("eve"),
                "eve.client": types.ModuleType("eve.client"),
                "eve.client.script": types.ModuleType("eve.client.script"),
                "eve.client.script.parklife": fake_states_pkg,
                "eve.client.script.parklife.states": fake_states,
                "const": fake_const,
            }), \
                 mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                 mock.patch.object(hook, "_install_warp_telemetry"), \
                 mock.patch.object(hook.H, "_get_beyonce", create=True, return_value=mock.Mock()), \
                 mock.patch.object(hook.H, "_sleep_ms", create=True), \
                 mock.patch.object(hook.H, "_sleep_sim_ms", create=True), \
                 mock.patch.object(
                     hook,
                     "_get_state_snapshot",
                     return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"},
                 ):
                res = hook._execute_action(fake_sm, {"type": "lock_target", "target": "Revelation", "wait_ms": 0})
                self.assertTrue(res["ok"], res)
                tgt_svc.TryLockTarget.assert_called_once_with(rev_id)

                tgt_svc.TryLockTarget.reset_mock()
                res_scroll = hook._execute_action(fake_sm, {"type": "lock_target", "target": "scrollentry_19720", "wait_ms": 0})
                self.assertTrue(res_scroll["ok"], res_scroll)
                tgt_svc.TryLockTarget.assert_called_once_with(rev_id)

                tgt_svc.TryLockTarget.reset_mock()
                res_missing = hook._execute_action(fake_sm, {"type": "lock_target", "target": "MissingTitan", "wait_ms": 0})
                self.assertFalse(res_missing["ok"], res_missing)
                self.assertIn("MissingTitan", res_missing["error"])
                tgt_svc.TryLockTarget.assert_not_called()
        finally:
            if orig_sess is None and hasattr(builtin_mod, "session"):
                delattr(builtin_mod, "session")
            elif orig_sess is not None:
                setattr(builtin_mod, "session", orig_sess)


class TestHookWindowResizeAndUiActions(unittest.TestCase):
    def test_hook_ensure_windowed_mode_resizes_up_and_avoids_redundant_ui_scale(self):
        import os
        hook = _load_hook_with_py2_stubs()
        with tempfile.TemporaryDirectory() as td:
            cfg_p = Path(td) / "inclient_cfg.json"
            cfg_p.write_text(json.dumps({
                "window_width": 3456,
                "window_height": 2112,
                "window_left": 0,
                "window_top": 0,
                "window_ui_scale": 2.0,
            }), encoding="utf-8")

            fake_ws = mock.Mock(windowMode=1, width=1280, height=800)
            copied_ws = mock.Mock()
            fake_ws.CopyTo.return_value = copied_ws
            fake_trinity = types.ModuleType("trinity")
            fake_trinity.Tr2WindowMode = types.SimpleNamespace(WINDOWED=1)
            fake_trinity.mainWindow = mock.Mock()
            fake_trinity.mainWindow.GetWindowState.return_value = fake_ws

            dev_svc = mock.Mock()
            dev_svc.GetUIScaleValue.return_value = 1.0
            fake_sm = mock.Mock()
            fake_sm.GetService.return_value = dev_svc

            orig_cached = hook._CACHED_WINDOW_CFG
            hook._CACHED_WINDOW_CFG = None
            try:
                with mock.patch.dict(os.environ, {"EVOMU_PLAYTESTER_CFG": str(cfg_p)}), \
                     mock.patch.dict(sys.modules, {"trinity": fake_trinity}), \
                     mock.patch.object(hook, "_disable_trinity_throttling"), \
                     mock.patch.object(hook.H, "_sleep_ms", create=True):
                    hook._ensure_windowed_mode(fake_sm)
                    dev_svc.SetWindowState.assert_called_once_with(copied_ws)
                    self.assertEqual(copied_ws.width, 3456)
                    self.assertEqual(copied_ws.height, 2112)
                    self.assertEqual(copied_ws.left, 0)
                    self.assertEqual(copied_ws.top, 0)
                    dev_svc.SetUIScaleValue.assert_called_once_with(2.0)

                    # Now simulate already at target size & scale -> no resize, no SetUIScaleValue
                    dev_svc.SetWindowState.reset_mock()
                    dev_svc.SetUIScaleValue.reset_mock()
                    fake_ws.width = 3456
                    fake_ws.height = 2112
                    dev_svc.GetUIScaleValue.return_value = 2.0
                    hook._ensure_windowed_mode(fake_sm)
                    dev_svc.SetWindowState.assert_not_called()
                    dev_svc.SetUIScaleValue.assert_not_called()
            finally:
                hook._CACHED_WINDOW_CFG = orig_cached

    def test_right_click_selects_scroll_node_before_show_menu(self):
        hook = _load_hook_with_py2_stubs()
        scroll = mock.Mock()
        sr_node = types.SimpleNamespace(scroll=scroll, selected=False)
        inv_item = mock.Mock()
        inv_item.children = []
        inv_item.sr = types.SimpleNamespace(node=sr_node)
        inv_item.name = "ItemEntry_434"
        inv_item.expandOnLeft = False
        inv_item.HasEventHandler.return_value = True

        fake_ctx_mod = types.ModuleType("carbonui.control.contextMenu.contextMenu")
        show_menu_calls = []

        def _fake_show_menu(obj):
            # Mirror EVE ItemEntry.GetMenu() expecting scroll selection before ShowMenu runs
            scroll.SelectNode.assert_called_once_with(sr_node)
            show_menu_calls.append(obj)

        fake_ctx_mod.ShowMenu = _fake_show_menu
        fake_util_mod = types.ModuleType("carbonui.control.contextMenu.menuUtil")
        fake_util_mod.CloseContextMenus = lambda: None
        fake_cui = types.ModuleType("carbonui")
        fake_ctrl = types.ModuleType("carbonui.control")
        fake_cm_pkg = types.ModuleType("carbonui.control.contextMenu")

        with mock.patch.dict(sys.modules, {
            "carbonui": fake_cui,
            "carbonui.control": fake_ctrl,
            "carbonui.control.contextMenu": fake_cm_pkg,
            "carbonui.control.contextMenu.contextMenu": fake_ctx_mod,
            "carbonui.control.contextMenu.menuUtil": fake_util_mod,
        }), mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False), \
             mock.patch.object(hook, "_resolve_editable_widget", return_value=None), \
             mock.patch.object(hook, "_get_uicore", return_value=None):
            out = hook._invoke_widget_click(inv_item, right_click=True)
        self.assertEqual(out, "ShowMenu(ItemEntry_434)")
        self.assertEqual(show_menu_calls, [inv_item])

    def test_resolve_ball_name_prefers_stargate_jump_destination(self):
        import builtins as builtin_mod

        hook = _load_hook_with_py2_stubs()
        slim = types.SimpleNamespace(
            groupID=10,
            name="Jita",
            jumps=[types.SimpleNamespace(locationID=30000144, toCelestialID=50001250)],
        )
        fake_evelocs = mock.Mock()
        fake_evelocs.GetIfExists.side_effect = lambda lid: types.SimpleNamespace(name="Perimeter") if lid == 30000144 else None
        fake_cfg = types.SimpleNamespace(evelocations=fake_evelocs)

        orig_cfg = getattr(builtin_mod, "cfg", None)
        had_cfg = hasattr(builtin_mod, "cfg")
        setattr(builtin_mod, "cfg", fake_cfg)
        try:
            self.assertEqual(hook._resolve_ball_name(slim, 50001248), "Stargate (Perimeter)")
        finally:
            if had_cfg:
                setattr(builtin_mod, "cfg", orig_cfg)
            else:
                delattr(builtin_mod, "cfg")

    def test_close_window_clicks_modal_dismiss_button_and_uses_id_cancel_fallback(self):
        hook = _load_hook_with_py2_stubs()

        class FakeButton(object):
            def __init__(self, label, name, on_click):
                self.label = label
                self.name = name
                self.children = []
                self.destroyed = False
                self._on_click = on_click

            def OnClick(self):
                self._on_click()

        class FakeSkillRequirementDialog(object):
            def __init__(self):
                self.windowID = "skill_requirement_dialog"
                self.name = "skill_requirement_dialog"
                self.caption = "Information"
                self.isModal = True
                self.destroyed = False
                self.modal_results = []
                self.close_by_user_called = False
                self.buy_btn = FakeButton("Buy and train", "BuyAndTrainButton", self._on_buy)
                self.later_btn = FakeButton("Maybe later", "Button", self._on_later)
                self.children = [self.buy_btn, self.later_btn]

            def _on_buy(self):
                self.result = True
                self.destroyed = True

            def _on_later(self):
                self.result = False
                self.destroyed = True

            def SetModalResult(self, val):
                self.modal_results.append(val)
                raise AssertionError("SetModalResult should not be forced when dismiss button closed dialog")

            def CloseByUser(self):
                self.close_by_user_called = True
                raise AssertionError("CloseByUser should not be called after Maybe later closed dialog")

        class FakeFallbackPopup(object):
            def __init__(self):
                self.windowID = "generic_popup"
                self.name = "generic_popup"
                self.caption = "Notice"
                self.isModal = True
                self.destroyed = False
                self.children = []
                self.modal_results = []

            def SetModalResult(self, val):
                self.modal_results.append(val)
                self.destroyed = True

        dlg = FakeSkillRequirementDialog()
        popup = FakeFallbackPopup()
        fake_reg = types.SimpleNamespace(GetWindows=lambda: [dlg, popup])
        fake_uicore = types.SimpleNamespace(registry=fake_reg, layer=None)
        fake_sm = mock.Mock()
        fake_uiconst = types.ModuleType("carbonui.uiconst")
        fake_uiconst.ID_CANCEL = 2

        with mock.patch.dict(sys.modules, {"carbonui.uiconst": fake_uiconst}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_is_visible", side_effect=lambda n: not getattr(n, "destroyed", False)), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            res_dlg = hook._execute_action(fake_sm, {"type": "close_window", "target": "skill_requirement_dialog", "wait_ms": 0})
            res_pop = hook._execute_action(fake_sm, {"type": "close_window", "target": "generic_popup", "wait_ms": 0})

        self.assertTrue(res_dlg.get("ok"), res_dlg)
        self.assertTrue(dlg.destroyed)
        self.assertIs(dlg.result, False)
        self.assertEqual(dlg.modal_results, [])
        self.assertFalse(dlg.close_by_user_called)

        self.assertTrue(res_pop.get("ok"), res_pop)
        self.assertTrue(popup.destroyed)
        self.assertEqual(popup.modal_results, [2])


class TestHookExecutionAndInclientHelpers(unittest.TestCase):
    def test_hook_settle_after_action_and_post_state_reuse(self):
        hook = _load_hook_with_py2_stubs()
        hook._LAST_SNAPSHOT = None
        hook._LAST_SNAPSHOT_SM = None
        hook._LAST_SNAPSHOT_TS = 0.0
        hook._MACHO_CALL_TASKLET_DEPTH.clear()

        sleep_calls = []
        snap_calls = []

        def fake_snap(sm):
            snap_calls.append(len(snap_calls) + 1)
            return {
                "open_windows": [{"window_id": "neocom", "name": "neocom"}],
                "modals": [],
                "ui": {"open_windows": [{"window_id": "neocom", "name": "neocom"}], "modals": [], "clickable_widgets": []},
                "space": {},
                "skills_state": {},
                "wallet_state": {},
                "active_view": "hangar",
            }

        fake_sm = mock.MagicMock()
        fake_uicore = types.SimpleNamespace(registry=types.SimpleNamespace(GetWindows=lambda: []), layer=None)
        with mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook, "_run_gm_slash", return_value={"ok": True}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_get_state_snapshot", side_effect=fake_snap), \
             mock.patch.object(hook.H, "_sleep_ms", create=True, side_effect=lambda ms: sleep_calls.append(ms)):
            res1 = hook._execute_action(fake_sm, {"type": "slash_command", "command": "/cleartimers", "wait_ms": 0})
            res2 = hook._execute_action(fake_sm, {"type": "slash_command", "command": "/cleartimers", "wait_ms": 0})

        self.assertTrue(res1.get("ok"))
        self.assertIn("post_state", res1)
        # First call takes pre + post snapshot (2); second call reuses _LAST_SNAPSHOT as pre_state (1 post)!
        self.assertEqual(len(snap_calls), 3)
        self.assertEqual(sleep_calls, [25, 25])

        # When Macho call depth > 0, _settle_after_action polls until depth drops to 0
        hook._MACHO_CALL_TASKLET_DEPTH[1] = 1
        poll_sleeps = []

        def fake_poll_sleep(ms):
            poll_sleeps.append(ms)
            if len(poll_sleeps) >= 3:
                hook._MACHO_CALL_TASKLET_DEPTH.clear()

        with mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": []}), \
             mock.patch.object(hook.H, "_sleep_ms", create=True, side_effect=fake_poll_sleep):
            hook._settle_after_action(fake_sm, 0, 0, min_settle_ms=0, max_ms=250)
        self.assertEqual(poll_sleeps, [25, 25, 25])

    def test_condition_aware_wait_early_exit_and_cap(self):
        hook = _load_hook_with_py2_stubs()
        hook._LAST_SNAPSHOT = None
        hook._LAST_SNAPSHOT_SM = None
        hook._LAST_SNAPSHOT_TS = 0.0
        fake_uicore = types.SimpleNamespace(layer=None)

        # 1. Normal non-strict wait(15000) is capped at 3000ms
        slept_norm = []
        fake_sm = mock.Mock()
        with mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook.H, "_sleep_ms", create=True, side_effect=lambda ms: slept_norm.append(ms)), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {"ego": {"mode": 2}}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight", "session": {"solarsystemid2": 30000142, "stationid": None}}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            res_cap = hook._execute_action(fake_sm, {"type": "wait", "wait_ms": 15000})
        self.assertEqual(res_cap.get("waited_ms"), 3000)
        self.assertEqual(sum(slept_norm), 3000)

        # 2. Warp wait (pre_mode == 3) exits early once mode transitions != 3
        warp_modes = [3, 3, 2]
        slept_warp = []

        def sample_ego(_sm):
            return {"mode": warp_modes.pop(0) if warp_modes else 2}

        fake_sm_warp = mock.Mock()
        with mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_sample_ego_warp_state", side_effect=sample_ego), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook.H, "_sleep_ms", create=True, side_effect=lambda ms: slept_warp.append(ms)), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {"ego": {"mode": 3}}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight", "session": {"solarsystemid2": 30000142, "stationid": None}}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            res_warp = hook._execute_action(fake_sm_warp, {"type": "wait", "wait_ms": 15000})
        self.assertEqual(res_warp.get("wait_early_exit"), "warp_finished")
        self.assertEqual(res_warp.get("waited_ms"), 600)
        self.assertLess(sum(slept_warp), 1000)

        # 3. Warp wait must NOT exit early when mode != 3 if signal_in_warp is still True
        warp_sig_samples = [
            {"mode": 2, "signal_in_warp": True},
            {"mode": 2, "signal_in_warp": True},
            {"mode": 2, "signal_in_warp": False},
        ]
        slept_sig = []

        def sample_ego_sig(_sm):
            return warp_sig_samples.pop(0) if warp_sig_samples else {"mode": 2, "signal_in_warp": False}

        with mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_sample_ego_warp_state", side_effect=sample_ego_sig), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook.H, "_sleep_ms", create=True, side_effect=lambda ms: slept_sig.append(ms)), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {"ego": {"mode": 2, "signal_in_warp": True}}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight", "session": {"solarsystemid2": 30000142, "stationid": None}}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            res_sig = hook._execute_action(fake_sm_warp, {"type": "wait", "wait_ms": 15000})
        self.assertEqual(res_sig.get("wait_early_exit"), "warp_finished")
        self.assertEqual(res_sig.get("waited_ms"), 600)
        self.assertEqual(sum(slept_sig), 600)

    def test_press_key_tab_without_editable_inputs_uses_cmd_or_noop(self):
        hook = _load_hook_with_py2_stubs()
        called = []

        class CmdWithTab:
            combatFunctionLoaded = None
            def OnTab(self):
                called.append("OnTab")
            def OnShiftTab(self):
                called.append("OnShiftTab")

        class CmdNoTab:
            combatFunctionLoaded = None

        fake_sm = mock.MagicMock()
        for cmd_svc, exp_tab, exp_shift in (
            (CmdWithTab(), "OnTab", "OnShiftTab"),
            (CmdNoTab(), "TAB(noop_no_visible_editable)", "SHIFT+TAB(noop_no_visible_editable)"),
        ):
            fake_uicore = types.SimpleNamespace(cmd=cmd_svc, registry=None, layer=None, desktop=None)
            with mock.patch.dict(sys.modules, {"uthread": None}), \
                 mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                 mock.patch.object(hook, "_install_warp_telemetry"), \
                 mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
                 mock.patch.object(hook.H, "_sleep_ms", create=True), \
                 mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
                 mock.patch.object(hook, "_iter_visible_layers", return_value=[]):
                res_tab = hook._execute_action(fake_sm, {"type": "press_key", "target": "TAB", "command": "", "wait_ms": 0})
                res_shift = hook._execute_action(fake_sm, {"type": "press_key", "target": "SHIFT+TAB", "command": "", "wait_ms": 0})
            self.assertTrue(res_tab.get("ok"), res_tab)
            self.assertEqual(res_tab.get("executed_cmd"), exp_tab)
            self.assertTrue(res_shift.get("ok"), res_shift)
            self.assertEqual(res_shift.get("executed_cmd"), exp_shift)
        self.assertEqual(called, ["OnTab", "OnShiftTab"])

    def test_stale_module_button_rejected_in_click_and_activate(self):
        hook = _load_hook_with_py2_stubs()
        clicked = []

        class FakeModuleButton:
            def __init__(self, name, item_id, type_id):
                self.name = name
                self.moduleinfo = types.SimpleNamespace(itemID=item_id, typeID=type_id)
                self.online = True

            def OnClick(self):
                clicked.append(self.name)

        fake_sess = types.SimpleNamespace(shipid=9001)
        # Case 1: Active ship in godma invItems is a Capsule (typeID == 670)
        cap_inv = {9001: types.SimpleNamespace(typeID=670), 5001: types.SimpleNamespace(typeID=483)}
        godma_cap = types.SimpleNamespace(stateManager=types.SimpleNamespace(invItems=cap_inv))
        sm_cap = mock.MagicMock()
        sm_cap.GetService.side_effect = lambda name: godma_cap if name == "godma" else None
        btn_cap = FakeModuleButton("ModuleButton_483", 5001, 483)

        # Case 2: Ship is not a capsule, but module itemID is purged from godma invItems
        purged_inv = {9001: types.SimpleNamespace(typeID=601)}
        godma_purged = types.SimpleNamespace(stateManager=types.SimpleNamespace(invItems=purged_inv))
        sm_purged = mock.MagicMock()
        sm_purged.GetService.side_effect = lambda name: godma_purged if name == "godma" else None
        btn_purged = FakeModuleButton("ModuleButton_483", 5001, 483)

        with mock.patch.object(hook.__builtin__, "session", fake_sess, create=True):
            for sm_obj, btn in ((sm_cap, btn_cap), (sm_purged, btn_purged)):
                with mock.patch.object(hook.__builtin__, "sm", sm_obj, create=True), \
                     mock.patch.object(hook, "_get_uicore", return_value=None), \
                     mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
                     mock.patch.object(hook, "_close_utilmenu_if_outside"), \
                     mock.patch.object(hook, "_close_context_menus_if_outside"), \
                     mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False):
                    out = hook._invoke_widget_click(btn)
                self.assertTrue(str(out).startswith("StaleModuleButton("), out)

                fake_uicore = types.SimpleNamespace(cmd=None, registry=None, layer=None, desktop=None)
                with mock.patch.dict(sys.modules, {"uthread": None}), \
                     mock.patch.object(hook.__builtin__, "sm", sm_obj, create=True), \
                     mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                     mock.patch.object(hook, "_install_warp_telemetry"), \
                     mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
                     mock.patch.object(hook.H, "_sleep_ms", create=True), \
                     mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
                     mock.patch.object(hook, "_find_widget", return_value=btn):
                    cw_res = hook._execute_action(sm_obj, {"type": "click_widget", "target": "ModuleButton_483", "wait_ms": 0})
                self.assertFalse(cw_res.get("ok"), cw_res)
                self.assertIn("StaleModuleButton(", cw_res.get("error", ""))

        self.assertEqual(clicked, [])

    def test_type_text_on_quick_filter_waits_for_debounce_settle(self):
        hook = _load_hook_with_py2_stubs()
        captured_settle = []

        class FakeFilterEdit:
            name = "quickFilterInputBox"
            def SetValue(self, val):
                self._val = val

        fake_uicore = types.SimpleNamespace(cmd=None, registry=None, layer=None, desktop=None)
        fake_sm = mock.MagicMock()
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_find_widget", return_value=FakeFilterEdit()), \
             mock.patch.object(hook, "_resolve_editable_widget", side_effect=lambda n: n), \
             mock.patch.object(hook, "_settle_after_action", side_effect=lambda *a, **kw: captured_settle.append(kw) or kw.get("min_settle_ms", 0)):
            res = hook._execute_action(fake_sm, {"type": "type_text", "target": "InvContQuickFilter/quickFilterInputBox", "command": "Merlin\n", "wait_ms": 250})
        self.assertTrue(res.get("ok"), res)
        self.assertEqual(len(captured_settle), 1)
        self.assertGreaterEqual(captured_settle[0]["min_settle_ms"], 180)

    def test_type_text_missing_command_does_not_type_widget_path(self):
        hook = _load_hook_with_py2_stubs()
        set_values = []

        class FakePriceEdit:
            name = "edit_price"
            def SetValue(self, val):
                set_values.append(val)

        fake_uicore = types.SimpleNamespace(cmd=None, registry=None, layer=None, desktop=None)
        fake_sm = mock.MagicMock()
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_find_widget", return_value=FakePriceEdit()):
            res = hook._execute_action(
                fake_sm,
                {"type": "type_text", "target": "main/l_main/createcontract/edit_price", "command": None, "wait_ms": 0},
            )
        self.assertFalse(res.get("ok"), res)
        self.assertIn("requires non-empty 'command' or 'text'", res.get("error", ""))
        self.assertEqual(set_values, [])

    def test_wait_checks_flight_recorder_without_name_error(self):
        hook = _load_hook_with_py2_stubs()
        fake_sm = mock.MagicMock()

        def fake_sleep(_ms):
            hook._record_flight("macho", "dogmaIM", "Activate", (), "ERR", 10.0, "UserError('Foo')")

        hook._FLIGHT_RECORDER.clear()
        hook._FLIGHT_SEQ = 0
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_sleep_ms", side_effect=fake_sleep, create=True):
            res = hook._execute_action(fake_sm, {"type": "wait", "wait_ms": 1000})
        self.assertTrue(res.get("ok"), res)
        self.assertEqual(res.get("wait_early_exit"), "flight_error")
        hook._FLIGHT_RECORDER.clear()

    def test_drag_widget_drop_user_error_reports_failure(self):
        hook = _load_hook_with_py2_stubs()
        fake_sm = mock.MagicMock()

        class UserError(Exception):
            def __init__(self, msg):
                super().__init__(msg)
                self.msg = msg

        class FakeDragItem:
            name = "ItemEntry_30013"
            def GetDragData(self):
                return ["drag_probe"]

        class FakeDropTarget:
            name = "ModuleButton_17938"
            def OnDropData(self, src, data):
                raise UserError("FittingHasSkillPrerequisites")

        hook._FLIGHT_RECORDER.clear()
        hook._FLIGHT_SEQ = 0
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_uicore", return_value=mock.MagicMock(layer=mock.MagicMock())), \
             mock.patch.object(hook, "_find_widget", return_value=FakeDragItem()), \
             mock.patch.object(hook, "_find_widget_at_xy", return_value=FakeDropTarget()), \
             mock.patch.object(hook, "_has_real_drop_handler", return_value=True), \
             mock.patch.object(hook, "_notify_user_error"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
             mock.patch.object(hook.H, "_sleep_ms", create=True):
            res = hook._execute_action(
                fake_sm,
                {"type": "drag_widget", "target": "ItemEntry_30013", "dest_x": 1042, "dest_y": 891},
            )
        self.assertFalse(res.get("ok"), res)
        self.assertFalse(res.get("action_ok"), res)
        self.assertEqual(res.get("drop_user_error"), "FittingHasSkillPrerequisites")
        self.assertIn("FittingHasSkillPrerequisites", res.get("error") or "")

    def test_click_inventory_stackall_waits_for_rebuild_settle(self):
        hook = _load_hook_with_py2_stubs()
        captured_settle = []

        class FakeStackAllBtn:
            name = "unique_UI_inventoryStackAll"
            def OnClick(self):
                pass

        fake_uicore = types.SimpleNamespace(cmd=None, registry=None, layer=None, desktop=None)
        fake_sm = mock.MagicMock()
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_find_widget", return_value=FakeStackAllBtn()), \
             mock.patch.object(hook, "_settle_after_action", side_effect=lambda *a, **kw: captured_settle.append(kw) or kw.get("min_settle_ms", 0)):
            res = hook._execute_action(fake_sm, {"type": "click_widget", "target": "unique_UI_inventoryStackAll", "wait_ms": 250})
        self.assertTrue(res.get("ok"), res)
        self.assertEqual(len(captured_settle), 1)
        self.assertGreaterEqual(captured_settle[0]["min_settle_ms"], 250)

    def test_open_client_window_retries_on_window_blocked_from_opening(self):
        hook = _load_hook_with_py2_stubs()
        calls = []
        sleeps = []

        class UserError(Exception):
            def __init__(self, msg):
                super().__init__(msg)
                self.msg = msg

        def open_fitting():
            calls.append("OpenFitting")
            if len(calls) == 1:
                raise UserError("WindowBlockedFromOpening")

        fake_cmd = types.SimpleNamespace(OpenFitting=open_fitting)
        fake_uicore = types.SimpleNamespace(cmd=fake_cmd, registry=types.SimpleNamespace(GetWindows=lambda: []))
        fake_sm = mock.MagicMock()
        with mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook.H, "_sleep_ms", create=True, side_effect=lambda ms: sleeps.append(ms)):
            res = hook._open_client_window(fake_sm, "fitting")
        self.assertTrue(res.get("ok"), res)
        self.assertEqual(calls, ["OpenFitting", "OpenFitting"])
        self.assertGreaterEqual(sum(sleeps), 1200)

    def test_stacked_modals_prefer_topmost_modal_in_find_and_close(self):
        hook = _load_hook_with_py2_stubs()

        class FakeNode:
            def __init__(self, name, cls_name, x, y, w, h, parent=None, label=""):
                self.name = name
                self._cls_name = cls_name
                self._rect = (x, y, w, h)
                self.parent = parent
                self.children = []
                self.display = True
                self.destroyed = False
                self.state = 0
                self.label = label
                if parent is not None:
                    parent.children.append(self)

            def GetAbsolute(self):
                return self._rect

            def HasEventHandler(self, evt):
                return evt == "OnClick" and callable(getattr(self, "OnClick", None))

        modal_layer = FakeNode("l_modal", "Layer", 0, 0, 1280, 800)
        # Index 0 is frontmost (top) modal; index 1 is older underlying modal.
        wrap_top = FakeNode("l_modal_modal", "Container", 0, 0, 1280, 800, parent=modal_layer)
        msg_top = FakeNode("modal", "MessageBox", 660, 404, 420, 260, parent=wrap_top)
        msg_top.isModal = True
        msg_top.windowID = "message"
        msg_top.caption = "Discard?"
        btn_top_no = FakeNode("no_dialog_button", "Button", 972, 638, 196, 32, parent=msg_top, label="No")

        wrap_old = FakeNode("l_modal_modal", "Container", 0, 0, 1280, 800, parent=modal_layer)
        msg_old = FakeNode("modal", "MessageBox", 644, 388, 420, 260, parent=wrap_old)
        msg_old.isModal = True
        msg_old.windowID = "message"
        msg_old.caption = "Discard?"
        btn_old_no = FakeNode("no_dialog_button", "Button", 956, 622, 196, 32, parent=msg_old, label="No")

        # Background non-modal window that must NOT be closed by close_window("").
        bg_chat = FakeNode("chatchannel_corp", "XmppChatWindow", 66, 704, 446, 314)
        bg_chat.windowID = "chatchannel_corp"
        bg_chat.CloseByUser = lambda: setattr(bg_chat, "destroyed", True)

        modals_stack = [msg_old, msg_top]

        def _dismiss(wnd):
            if modals_stack and modals_stack[-1] is wnd:
                wnd.destroyed = True
                modals_stack.remove(wnd)

        btn_top_no.OnClick = lambda: _dismiss(msg_top)
        btn_old_no.OnClick = lambda: _dismiss(msg_old)

        class FakeReg:
            def GetModalWindow(self):
                return modals_stack[-1] if modals_stack else None

            @property
            def modals(self):
                return list(modals_stack)

            def GetWindows(self):
                return [bg_chat, msg_old, msg_top]

        fake_layers = types.SimpleNamespace(modal=modal_layer, abovemain=None, main=None)
        fake_uicore = types.SimpleNamespace(layer=fake_layers, desktop=None, registry=FakeReg())
        fake_sm = mock.Mock()
        fake_sm.GetService.return_value = None

        with mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_is_visible", side_effect=lambda n: not getattr(n, "destroyed", False)), \
             mock.patch.object(hook, "_is_pickable", side_effect=lambda n: not getattr(n, "destroyed", False)), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}):
            # Overlapping coordinate (980, 645) belongs to BOTH buttons; must resolve topmost.
            found_by_name = hook._find_widget("no_dialog_button", fake_layers, hint_x=980, hint_y=645)
            found_by_xy = hook._find_widget_at_xy(980, 645, fake_layers)
            self.assertIs(found_by_name, btn_top_no)
            self.assertIs(found_by_xy, btn_top_no)

            res = hook._execute_action(fake_sm, {"type": "close_window", "target": "", "wait_ms": 0})

        self.assertTrue(res.get("ok"), res)
        self.assertTrue(msg_top.destroyed)
        self.assertTrue(msg_old.destroyed)
        self.assertFalse(bg_chat.destroyed)

    def test_click_alias_and_unknown_atype_rejected(self):
        hook = _load_hook_with_py2_stubs()
        clicked = []

        class FakeBtn:
            name = "repairshop"
            def OnClick(self):
                clicked.append("repairshop")

        fake_uicore = types.SimpleNamespace(cmd=None, registry=None, layer=None, desktop=None)
        fake_sm = mock.MagicMock()
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_find_widget", return_value=FakeBtn()), \
             mock.patch.object(hook, "_settle_after_action", return_value=25):
            res_click = hook._execute_action(fake_sm, {"type": "click", "target": "repairshop", "wait_ms": 0})
            res_bad = hook._execute_action(fake_sm, {"type": "bogus_noop_action", "target": "x", "wait_ms": 0})
        self.assertTrue(res_click.get("ok"), res_click)
        self.assertEqual(clicked, ["repairshop"])
        self.assertFalse(res_bad.get("ok"), res_bad)
        self.assertIn("unsupported action type", res_bad.get("error", ""))

    def test_wait_action_resolves_numeric_target_over_default_wait_ms(self):
        hook = _load_hook_with_py2_stubs()
        fake_sm = mock.MagicMock()
        slept = []
        hook._FLIGHT_RECORDER.clear()
        hook._FLIGHT_SEQ = 0
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_sleep_ms", side_effect=lambda ms: slept.append(ms), create=True):
            res = hook._execute_action(fake_sm, {"type": "wait", "target": "1500", "wait_ms": 250})
        self.assertTrue(res.get("ok"), res)
        self.assertEqual(res.get("waited_ms"), 1500)
        self.assertEqual(sum(slept), 1500)

    def test_wait_warp_fx_settled_drains_sleepsim_when_hooked_and_warped(self):
        import types
        import unittest.mock as mock
        helpers = _load_helpers_with_py2_stubs()

        sim_sleeps = []
        mode_seq = iter([3, 3, 2, 2, 2, 2])

        class _Ball:
            @property
            def mode(self):
                return next(mode_seq, 2)

        class _BP:
            def GetBall(self, _sid):
                return _Ball()

        class _Michelle:
            def GetBallpark(self):
                return _BP()

        class _Space:
            warpDestinationCache = None

        class _TC:
            def AdvanceClock(self, _secs):
                return True

        class _SM:
            services = {"michelle": _Michelle(), "space": _Space()}

            def RemoteSvc(self, _name):
                return _TC()

        fake_sess = types.SimpleNamespace(shipid=1897073087, stationid=None, structureid=None)
        orig_state = dict(helpers._WARP_SIGNAL_STATE)
        try:
            helpers._WARP_SIGNAL_STATE.update({"hooked": True, "in_warp": False, "started": 3, "finished": 3})
            with mock.patch.object(helpers, "_ensure_warp_signal_tracker", return_value=True), \
                 mock.patch.object(helpers, "_sleep_sim_ms", side_effect=lambda ms: sim_sleeps.append(ms)), \
                 mock.patch.object(helpers, "_sleep_ms", return_value=None), \
                 mock.patch.object(helpers.__builtin__, "session", fake_sess, create=True):
                helpers._wait_warp_fx_settled(_SM(), timeout_sec=1.0)
        finally:
            helpers._WARP_SIGNAL_STATE.clear()
            helpers._WARP_SIGNAL_STATE.update(orig_state)

        self.assertGreaterEqual(len(sim_sleeps), 4)
        self.assertEqual(sim_sleeps[-1], 600)


class TestHookWindowCommandsAndRpcCompat(unittest.TestCase):
    @staticmethod
    def _find_client_identifiers_path():
        candidates = [REPO_ROOT / ".evomu-reviews" / "client_identifiers.json"]
        try:
            common = subprocess.check_output(
                ["git", "rev-parse", "--git-common-dir"],
                cwd=str(REPO_ROOT),
                text=True,
                timeout=3,
            ).strip()
            if common:
                common_path = Path(common)
                if not common_path.is_absolute():
                    common_path = (REPO_ROOT / common_path).resolve()
                candidates.append(common_path.parent / ".evomu-reviews" / "client_identifiers.json")
        except Exception:
            pass
        for cand in candidates:
            if cand.is_file() and cand.stat().st_size > 0:
                return cand
        return None

    def test_window_command_and_shortcut_tables_exist_in_client_identifiers(self):
        hook_src = (HARNESS_DIR / "evomu_playtester_hook.py").read_text(encoding="utf-8")
        tree = ast.parse(hook_src, filename="evomu_playtester_hook.py")
        tables = {}
        for node in ast.walk(tree):
            if isinstance(node, ast.Assign):
                for tgt in node.targets:
                    if isinstance(tgt, ast.Name) and tgt.id in ("cmd_map", "key_cmd_map"):
                        tables[tgt.id] = ast.literal_eval(node.value)
        self.assertIn("cmd_map", tables)
        self.assertIn("key_cmd_map", tables)

        forbidden_stale = {
            "OpenAgentFinder",
            "CmdToggleAirCareerProgram",
            "OpenLocationsWindow",
            "OpenConstellationMap",
            "CmdOpenScanner",
            "OpenScanner",
            "CmdOpenDirectionalScanner",
            "CmdOpenFleetFinder",
        }
        all_entries = []
        for tbl_name, tbl in tables.items():
            for key, raw_val in tbl.items():
                vals = raw_val if isinstance(raw_val, (tuple, list)) else (raw_val,)
                for val in vals:
                    all_entries.append((tbl_name, key, val))
                    self.assertNotIn(
                        val,
                        forbidden_stale,
                        f"Stale non-client identifier {val!r} found in {tbl_name}[{key!r}]",
                    )

        cid_path = self._find_client_identifiers_path()
        if cid_path is not None:
            raw = json.loads(cid_path.read_text(encoding="utf-8"))
            known = set(raw.get("identifiers", raw) if isinstance(raw, dict) else raw)
            missing = [
                f"{tbl_name}[{key!r}] -> {val!r}"
                for tbl_name, key, val in all_entries
                if val not in known
            ]
            self.assertEqual(
                missing,
                [],
                f"Window/shortcut command identifier(s) missing from {cid_path}: {missing}",
            )

    def test_open_client_window_uses_valid_v24_commands_for_agent_finder_acp_and_locations(self):
        hook = _load_hook_with_py2_stubs()
        calls = []

        class FakeCmdSvc:
            def OpenAgencyContentGroup(self, content_group_id):
                calls.append(("OpenAgencyContentGroup", int(content_group_id)))

            def OpenAchievementsWindow(self):
                calls.append(("OpenAchievementsWindow",))

            def OpenLocations(self):
                calls.append(("OpenLocations",))

        fake_uicore = types.SimpleNamespace(
            cmd=FakeCmdSvc(),
            registry=types.SimpleNamespace(GetWindows=lambda: []),
        )
        fake_sm = mock.MagicMock()
        with mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook.H, "_sleep_ms", create=True):
            for target in ("agentfinder", "Agent Finder", "aircareerprogram", "acp", "locations", "bookmarks"):
                res = hook._open_client_window(fake_sm, target)
                self.assertTrue(res.get("ok"), (target, res))

        self.assertEqual(
            calls,
            [
                ("OpenAgencyContentGroup", 28),
                ("OpenAgencyContentGroup", 28),
                ("OpenAchievementsWindow",),
                ("OpenAchievementsWindow",),
                ("OpenLocations",),
                ("OpenLocations",),
            ],
        )

    def test_rpc_dispatch_supports_all_main_host_commands(self):
        hook = _load_hook_with_py2_stubs()
        fake_sock = mock.MagicMock()
        fake_sock.getsockname.return_value = ("127.0.0.1", 26099)
        fake_sm = mock.MagicMock()
        fake_sess = types.SimpleNamespace(charid=90000001, stationid=60003760, solarsystemid=30000142)
        fake_snap = {
            "session": {"charid": 90000001, "stationid": 60003760, "solarsystemid": 30000142},
            "active_view": "hangar",
            "open_windows": [],
            "modals": [],
            "ui": {},
            "space": {},
            "skills_state": {},
            "wallet_state": {},
        }

        with mock.patch.object(hook.socket, "socket", return_value=fake_sock), \
             mock.patch.dict(sys.modules, {"evomu_site": types.SimpleNamespace(_EXC_SEQ=0, _RECENT_EXCEPTIONS=[])}):
            srv = hook.PlaytesterRpcServer({"playtester_port": 26099}, fake_sm)

        ping_resp = srv._dispatch({"method": "ping", "params": {}})
        self.assertTrue(ping_resp.get("ok"))
        self.assertTrue(ping_resp["result"].get("pong"))

        with mock.patch.dict(builtins.__dict__, {"session": fake_sess}), \
             mock.patch.object(hook, "_ensure_windowed_mode"), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value=fake_snap):
            boot_resp = srv._dispatch({"method": "bootstrap_session", "params": {"timeout_ms": 1000}})
            state_resp = srv._dispatch({"method": "get_state", "params": {}})
        self.assertTrue(boot_resp.get("ok"))
        self.assertEqual(boot_resp["result"]["session"]["charid"], 90000001)
        self.assertEqual(boot_resp["result"]["active_view"], "hangar")
        self.assertTrue(state_resp.get("ok"))
        self.assertEqual(state_resp["result"], fake_snap)

        with mock.patch.object(
            hook,
            "_capture_trinity_screenshot",
            return_value={"ok": True, "mode": "trinity_save_render_target", "path": "/tmp/s.png"},
        ):
            cap_resp = srv._dispatch({"method": "capture_screenshot", "params": {"path": "/tmp/s.png"}})
        self.assertTrue(cap_resp.get("ok"))
        self.assertEqual(cap_resp["result"]["mode"], "trinity_save_render_target")

        with mock.patch.object(
            hook,
            "_execute_action",
            return_value={"ok": True, "action_ok": True, "action_error": None, "flight_recorder": {}},
        ):
            act_resp = srv._dispatch({"method": "execute_action", "params": {"action": {"type": "wait", "wait_ms": 0}}})
        self.assertTrue(act_resp.get("ok"))
        self.assertTrue(act_resp["result"].get("action_ok"))

        # reload_hook executes reload(mod) when sys.modules[__name__] is registered
        fake_mod = sys.modules[hook.__name__] = types.ModuleType(hook.__name__)
        fake_mod.__dict__.update(hook.__dict__)
        with mock.patch.dict(builtins.__dict__, {"reload": lambda m: m}, create=True), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"):
            reload_resp = srv._dispatch({"method": "reload_hook", "params": {}})
        sys.modules.pop(hook.__name__, None)
        self.assertTrue(reload_resp.get("ok"))
        self.assertTrue(reload_resp.get("reloaded"))

        shut_resp = srv._dispatch({"method": "shutdown", "params": {}})
        self.assertTrue(shut_resp.get("ok"))
        self.assertTrue(shut_resp["result"].get("shutting_down"))
        self.assertFalse(srv.running)

    def test_execute_action_waits_for_session_mutating_and_changing_to_settle(self):
        """Regression for #1215: wait for ship.Board session mutation before OpenFitting."""
        hook = _load_hook_with_py2_stubs()
        sess = types.SimpleNamespace(
            shipid=None,
            mutating=1,
            changing="shipid",
            stationid=60003760,
            solarsystemid2=30000142,
        )
        observed_ship_ids = []

        def fake_open_fitting():
            observed_ship_ids.append((sess.shipid, sess.mutating, sess.changing))

        fake_cmd = types.SimpleNamespace(OpenFitting=fake_open_fitting)
        fake_uicore = types.SimpleNamespace(cmd=fake_cmd, registry=None, layer=None, desktop=None)
        fake_sm = mock.MagicMock()

        sleep_calls = []

        def fake_sleep(ms):
            sleep_calls.append(ms)
            if len(sleep_calls) == 2:
                sess.changing = None
            elif len(sleep_calls) >= 3:
                sess.shipid = 1000000001234
                sess.mutating = 0

        snap = {
            "open_windows": [],
            "modals": [],
            "ui": {},
            "space": {},
            "skills_state": {},
            "wallet_state": {},
            "active_view": "hangar",
        }
        with mock.patch.dict(builtins.__dict__, {"session": sess}), \
             mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value=snap), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False), \
             mock.patch.object(hook.H, "_sleep_ms", side_effect=fake_sleep, create=True):
            res = hook._execute_action(
                fake_sm,
                {"type": "open_neocom", "target": "fitting", "wait_ms": 0},
            )

        self.assertTrue(res.get("action_ok"), res)
        self.assertEqual(observed_ship_ids, [(1000000001234, 0, None)])
        self.assertEqual(sleep_calls[:3], [50, 50, 50])

        # Normal settled session executes immediately without extra _wait_session_settled polling,
        # and a permanently wedged session respects the bounded timeout without spinning forever.
        settled_sleeps = []
        settled_sess = types.SimpleNamespace(shipid=1000000001234, mutating=0, changing=None)
        with mock.patch.dict(builtins.__dict__, {"session": settled_sess}), \
             mock.patch.object(hook.H, "_sleep_ms", side_effect=lambda ms: settled_sleeps.append(ms), create=True):
            self.assertTrue(hook._wait_session_settled(timeout_ms=500, poll_ms=50))
        self.assertEqual(settled_sleeps, [])

        wedged_sleeps = []
        wedged_sess = types.SimpleNamespace(shipid=None, mutating=1, changing="shipid")
        with mock.patch.dict(builtins.__dict__, {"session": wedged_sess}), \
             mock.patch.object(hook.H, "_sleep_ms", side_effect=lambda ms: wedged_sleeps.append(ms), create=True):
            self.assertFalse(hook._wait_session_settled(timeout_ms=200, poll_ms=50))
        self.assertEqual(wedged_sleeps, [50, 50, 50, 50])


class TestScenarioHookExtensions(unittest.TestCase):
    def test_notifications_drawer_open_and_close_plus_content_and_charsel_inspection(self):
        hook = _load_hook_with_py2_stubs()
        drawer_state = {"open": False}

        class FakeLbl:
            def __init__(self, text):
                self.text = text
                self.name = "lbl"
                self.display = True
                self.destroyed = False

        class FakeScroll:
            def GetNodes(self):
                return [types.SimpleNamespace(label="Reward Earned"), types.SimpleNamespace(label="Skill Completed")]

        class FakeNotifCenter:
            def __init__(self):
                self.drawer = types.SimpleNamespace(
                    name="NotificationCenterDrawer",
                    display=True,
                    destroyed=False,
                    children=[FakeScroll(), FakeLbl("Notifications")],
                    scroll=FakeScroll(),
                )

            @property
            def isShowingDrawer(self):
                return drawer_state["open"]

            def OpenDrawer(self):
                drawer_state["open"] = True

            def CloseDrawer(self):
                drawer_state["open"] = False

        nc = FakeNotifCenter()
        fake_sm = types.SimpleNamespace(services={"notificationUIService": types.SimpleNamespace(notificationCenter=nc)})
        fake_uicore = types.SimpleNamespace(
            cmd=types.SimpleNamespace(),
            registry=types.SimpleNamespace(GetWindows=lambda: [], GetModalWindow=lambda: None, modals=[]),
            layer=types.SimpleNamespace(modal=None, abovemain=None, main=None),
            desktop=types.SimpleNamespace(width=1280, height=800),
        )
        with mock.patch.dict(builtins.__dict__, {"sm": fake_sm}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"):
            open_res = hook._open_client_window(fake_sm, "notifications")
            self.assertTrue(open_res.get("ok"), open_res)
            self.assertTrue(drawer_state["open"])

            ui_tree = hook._inspect_uicore_tree()
            wins = ui_tree.get("open_windows") or []
            notif_wins = [w for w in wins if "notification" in str(w.get("window_id") or "").lower()]
            self.assertEqual(len(notif_wins), 1, wins)
            self.assertGreaterEqual(int(notif_wins[0].get("populated_count", 0)), 1)
            self.assertFalse(notif_wins[0].get("has_loading"))
            self.assertFalse(notif_wins[0].get("has_error"))

            snap = {
                "open_windows": [], "modals": [], "ui": {}, "space": {},
                "skills_state": {}, "wallet_state": {}, "active_view": "hangar",
            }
            with mock.patch.dict(builtins.__dict__, {"session": types.SimpleNamespace(shipid=1001, mutating=0)}), \
                 mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                 mock.patch.object(hook, "_install_warp_telemetry"), \
                 mock.patch.object(hook, "_get_state_snapshot", return_value=snap):
                close_res = hook._execute_action(fake_sm, {"type": "close_window", "target": "notifications", "wait_ms": 0})
            self.assertTrue(close_res.get("action_ok"), close_res)
            self.assertFalse(drawer_state["open"])

    def test_defer_char_select_pauses_without_select_character_and_inspect_charsel_lists_chars(self):
        helpers = _load_helpers_with_py2_stubs()
        hook = _load_hook_with_py2_stubs()

        char_selected_calls = []

        class FakeCharSelLayer:
            def __init__(self):
                self.isopen = True
                self.display = True
                self.slots = [
                    types.SimpleNamespace(charDetails=types.SimpleNamespace(charID=90000001, characterName="Aura Capsuleer")),
                    types.SimpleNamespace(charDetails=types.SimpleNamespace(charID=90000002, characterName="Aura Two")),
                ]

            def SelectCharacterID(self, cid):
                char_selected_calls.append(cid)

        charsel_layer = FakeCharSelLayer()
        fake_sm_svcs = {
            "machoNet": object(),
            "gameui": types.SimpleNamespace(),
            "viewState": types.SimpleNamespace(
                GetCurrentViewInfo=lambda: types.SimpleNamespace(name="charsel"),
                isOpeningView=None,
            ),
        }
        fake_sm = types.SimpleNamespace(
            services=fake_sm_svcs,
            GetService=lambda name: fake_sm_svcs.get(name),
        )
        fake_uicore = types.SimpleNamespace(layer=types.SimpleNamespace(charsel=charsel_layer))
        fake_sess = types.SimpleNamespace(userid=1, charid=None, stationid=None, solarsystemid=None)

        with mock.patch.dict(builtins.__dict__, {"uicore": fake_uicore, "session": fake_sess}), \
             mock.patch.object(helpers, "_sleep_ms"):
            cid = helpers._wait_for_session(fake_sm, "demo", "demo-secret", 90000001, timeout_sec=2.0, defer_char_select=True)
        self.assertEqual(cid, 0)
        self.assertEqual(char_selected_calls, [])

        with mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            cs = hook._inspect_charsel_state(fake_sm)
        self.assertTrue(cs["active"])
        self.assertEqual(cs["names"], ["Aura Capsuleer", "Aura Two"])
        self.assertEqual([c["character_id"] for c in cs["characters"]], [90000001, 90000002])

    def test_notification_center_empty_drawer_reports_zero_populated_count(self):
        hook = _load_hook_with_py2_stubs()
        nc_container = types.SimpleNamespace(display=True, destroyed=False, children=[])
        nc = type("NotificationCenter", (), {
            "isShowingDrawer": True,
            "notificationContainer": nc_container,
        })()
        fake_sm = types.SimpleNamespace(
            services={"notificationUIService": types.SimpleNamespace(notificationCenter=nc)},
        )
        fake_desktop = types.SimpleNamespace(children=[], width=1280, height=720)
        fake_uicore = types.SimpleNamespace(desktop=fake_desktop, layer=types.SimpleNamespace(l_main=types.SimpleNamespace(children=[])))
        with mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.dict(builtins.__dict__, {"sm": fake_sm}):
            ui_tree = hook._inspect_uicore_tree()
        wins = [w for w in (ui_tree.get("open_windows") or []) if w.get("window_id") == "notifications"]
        self.assertEqual(len(wins), 1, ui_tree)
        self.assertTrue(wins[0]["is_showing_drawer"])
        self.assertEqual(wins[0]["populated_count"], 0)

        nc_container.children = [types.SimpleNamespace(display=True, destroyed=False, text="Skill completed", children=[])]
        with mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.dict(builtins.__dict__, {"sm": fake_sm}):
            ui_tree_pop = hook._inspect_uicore_tree()
        wins_pop = [w for w in (ui_tree_pop.get("open_windows") or []) if w.get("window_id") == "notifications"]
        self.assertEqual(len(wins_pop), 1)
        self.assertGreaterEqual(wins_pop[0]["populated_count"], 1)

    def test_inventory_state_populates_ship_hangar_count(self):
        hook = _load_hook_with_py2_stubs()
        fake_sess = types.SimpleNamespace(
            userid=1, charid=90000001, stationid=60003760, structureid=None,
            solarsystemid=None, solarsystemid2=30000142, shipid=1001,
        )
        fake_inv_cache = types.SimpleNamespace(inventories={
            (60003760, 4): types.SimpleNamespace(items={
                11: {"categoryID": 6},
                12: types.SimpleNamespace(categoryID=6),
                13: {"categoryID": 4},
            }),
        })
        fake_sm = types.SimpleNamespace(
            services={"invCache": fake_inv_cache},
            GetService=lambda n: fake_sm.services.get(n),
        )
        with mock.patch.dict(builtins.__dict__, {"session": fake_sess, "sm": fake_sm}), \
             mock.patch.object(hook, "_ensure_windowed_mode"), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_inspect_uicore_tree", return_value={"open_windows": [], "modals": []}):
            snap = hook._get_state_snapshot(fake_sm)
            self.assertEqual(snap["inventory_state"]["hangar_item_count"], 3)
            self.assertEqual(snap["inventory_state"]["ship_hangar_count"], 2)

            fake_inv_cache.inventories[(60003760, 4)] = types.SimpleNamespace(items={})
            snap_empty = hook._get_state_snapshot(fake_sm)
            self.assertEqual(snap_empty["inventory_state"]["hangar_item_count"], 0)
            self.assertEqual(snap_empty["inventory_state"]["ship_hangar_count"], 0)

    def test_space_command_stop_skips_ball_resolution_and_set_destination_handles_unicode(self):
        hook = _load_hook_with_py2_stubs()
        cmd_stop_calls = []
        waypoint_calls = []
        fake_beyonce = types.SimpleNamespace(CmdStop=lambda: cmd_stop_calls.append(True))
        fake_michelle = types.SimpleNamespace(GetBallpark=lambda: types.SimpleNamespace(
            GetBall=lambda bid: types.SimpleNamespace(mode=2),
        ))
        fake_starmap = types.SimpleNamespace(
            SetWaypoint=lambda did, clearOtherWaypoints=True: waypoint_calls.append((did, clearOtherWaypoints)),
            GetDestinationPath=lambda: [30000142],
        )
        fake_svcs = {"michelle": fake_michelle, "starmap": fake_starmap, "map": None}
        fake_sm = types.SimpleNamespace(services=fake_svcs, GetService=lambda n: fake_svcs.get(n))
        fake_sess = types.SimpleNamespace(userid=1, charid=90000001, solarsystemid=30000142, stationid=None, structureid=None, shipid=1001)
        fake_cfg = types.SimpleNamespace(evelocations=types.SimpleNamespace(data={
            30000142: types.SimpleNamespace(locationName="Jit\u00e4"),
        }))
        snap_base = {"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.dict(builtins.__dict__, {"session": fake_sess, "cfg": fake_cfg}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value=snap_base), \
             mock.patch.object(hook, "_settle_after_action", return_value=0), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook.H, "_get_beyonce", return_value=fake_beyonce, create=True), \
             mock.patch.object(hook, "_resolve_space_command_item_id", side_effect=AssertionError("stop must skip ball resolution")):
            res_stop = hook._execute_action(fake_sm, {"type": "stop", "wait_ms": 0, "reuse_pre_state": False})
            res_dest = hook._execute_action(fake_sm, {"type": "set_destination", "target": "Jit\u00e4", "wait_ms": 0, "reuse_pre_state": False})
        self.assertTrue(res_stop.get("ok"), res_stop)
        self.assertEqual(cmd_stop_calls, [True])
        self.assertTrue(res_dest.get("ok"), res_dest)
        self.assertEqual(res_dest.get("destination_id"), 30000142)
        self.assertEqual(waypoint_calls, [(30000142, True)])

    def test_execute_action_uses_25s_max_wait_for_select_character_and_warp_variants(self):
        hook = _load_hook_with_py2_stubs()
        snap_base = {"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}
        fake_uicore = types.SimpleNamespace(cmd=None, registry=None, layer=None, desktop=None)
        fake_sm = types.SimpleNamespace(services={}, GetService=lambda _n: None)
        fake_uthread = types.SimpleNamespace(new=lambda _fn: None)
        for atype in ("select_character", "warp_to_location", "warp_to_bookmark"):
            time_seq = iter([0.0, 0.0, 0.1, 20.0, 20.1, 26.0, 26.0, 26.0])
            with mock.patch.dict(sys.modules, {"uthread": fake_uthread}), \
                 mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                 mock.patch.object(hook, "_install_warp_telemetry"), \
                 mock.patch.object(hook, "_get_state_snapshot", return_value=snap_base), \
                 mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
                 mock.patch.object(hook, "_settle_after_action", return_value=0), \
                 mock.patch.object(hook.H, "_sleep_ms", create=True), \
                 mock.patch.object(hook.time, "time", side_effect=lambda s=time_seq: next(s, 26.0)):
                res = hook._execute_action(fake_sm, {"type": atype, "target": "x", "wait_ms": 0, "reuse_pre_state": False})
            self.assertFalse(res.get("ok"), f"{atype} unexpectedly succeeded: {res}")
            self.assertIn("timed out after 26.0s", res.get("error", ""), f"{atype}: {res}")

        # dock uses a 38.0s tasklet ceiling (> 35.0s scenario_mode warp budget, < 45.0s RPC socket timeout):
        # a worker finishing at 34.0s succeeds rather than timing out at the old 32.0s limit, while 38.5s times out.
        dock_seq = iter([0.0, 0.0, 34.0, 39.0, 39.0, 39.0])
        with mock.patch.dict(sys.modules, {"uthread": fake_uthread}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value=snap_base), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_settle_after_action", return_value=0), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook.time, "time", side_effect=lambda s=dock_seq: next(s, 39.0)):
            dock_res = hook._execute_action(fake_sm, {"type": "dock", "target": "60003760", "wait_ms": 0, "reuse_pre_state": False})
        self.assertFalse(dock_res.get("ok"), dock_res)
        self.assertIn("timed out after 39.0s", dock_res.get("error", ""), dock_res)

    def test_helpers_suppress_time_control_when_disabled(self):
        helpers = _load_helpers_with_py2_stubs()
        tc_calls = []

        class FakeTimeControl:
            def ClearSessionTimer(self):
                tc_calls.append("ClearSessionTimer")
            def AdvanceClock(self, secs):
                tc_calls.append(("AdvanceClock", secs))

        sess = types.SimpleNamespace(
            userid=1, charid=90000001, solarsystemid=None, solarsystemid2=30000142,
            stationid=60003760, structureid=None, shipid=1001, mutating=0, changing=None,
        )

        def _fake_undock():
            sess.stationid = None
            sess.solarsystemid = 30000142

        def _fake_dock(_sid):
            sess.stationid = 60003760
            sess.solarsystemid = None

        fake_ship_svc = types.SimpleNamespace(Undock=_fake_undock)
        fake_station_svc = types.SimpleNamespace(Dock=_fake_dock)
        fake_svcs = {"timeControl": FakeTimeControl(), "ship": fake_ship_svc, "stationSvc": fake_station_svc}
        fake_sm = types.SimpleNamespace(
            services=fake_svcs,
            GetService=lambda n: fake_svcs.get(n),
            RemoteSvc=lambda n: fake_svcs[n],
        )
        with mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm}), \
             mock.patch.object(helpers, "_wait_view_settled"), \
             mock.patch.object(helpers, "_ensure_view"), \
             mock.patch.object(helpers, "_ensure_warp_signal_tracker"), \
             mock.patch.object(helpers, "_sleep_ms"), \
             mock.patch.object(helpers, "_sleep_sim_ms"):
            helpers._undock_ship(fake_sm, allow_time_control=False)
            helpers._wait_warp_fx_settled(fake_sm, timeout_sec=0.05, allow_time_control=False)
            helpers._dock_station(fake_sm, 60003760, allow_time_control=False)
            self.assertEqual(tc_calls, [])

            helpers._undock_ship(fake_sm, allow_time_control=True)
            self.assertGreater(len(tc_calls), 0)

    def test_execute_action_suppresses_time_control_for_dock_and_jump_gate_in_scenario_mode(self):
        hook = _load_hook_with_py2_stubs()
        helpers = _load_helpers_with_py2_stubs()
        tc_calls = []

        class FakeTimeControl:
            def ClearSessionTimer(self):
                tc_calls.append("ClearSessionTimer")
            def AdvanceClock(self, secs):
                tc_calls.append(("AdvanceClock", secs))

        sess = types.SimpleNamespace(
            userid=1, charid=90000001, solarsystemid=30000142, solarsystemid2=30000142,
            stationid=None, structureid=None, shipid=1001, mutating=0, changing=None,
        )
        follow_calls = []

        def _fake_follow(tid, _dist):
            follow_calls.append(tid)

        def _fake_dock(st_id):
            sess.stationid = st_id
            sess.solarsystemid = None

        def _fake_jump(_gate_id):
            sess.solarsystemid = 30001363
            sess.solarsystemid2 = 30001363

        fake_beyonce = types.SimpleNamespace(
            CmdFollowBall=_fake_follow,
            CmdWarpToStuff=lambda *_a: None,
            CmdStargateJump=_fake_jump,
        )
        fake_station_svc = types.SimpleNamespace(Dock=_fake_dock)
        fake_bp = types.SimpleNamespace(
            GetBall=lambda bid: types.SimpleNamespace(surfaceDist=1000.0, mode=2),
            GetSurfaceDist=lambda _a, _b: 1000.0,
            slimItems={50001234: types.SimpleNamespace(groupID=10, name="Stargate (Sobaseki)")},
        )
        fake_michelle = types.SimpleNamespace(GetBallpark=lambda: fake_bp)
        fake_svcs = {"timeControl": FakeTimeControl(), "stationSvc": fake_station_svc, "michelle": fake_michelle}
        fake_sm = types.SimpleNamespace(
            services=fake_svcs,
            GetService=lambda n: fake_svcs.get(n),
            RemoteSvc=lambda n: fake_svcs[n],
        )
        snap_base = {"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value=snap_base), \
             mock.patch.object(hook, "_settle_after_action", return_value=0), \
             mock.patch.object(hook.H, "_sleep_ms", helpers._sleep_ms, create=True), \
             mock.patch.object(hook.H, "_sleep_sim_ms", lambda _ms: None, create=True), \
             mock.patch.object(hook.H, "_wait_view_settled", lambda _sm: None, create=True), \
             mock.patch.object(hook.H, "_ensure_view", lambda _sm, _v: None, create=True), \
             mock.patch.object(hook.H, "_wait_warp_fx_settled", helpers._wait_warp_fx_settled, create=True), \
             mock.patch.object(hook.H, "_wait_session_timer_clear", lambda max_wait_sec=12.0: None, create=True), \
             mock.patch.object(hook.H, "_get_beyonce", return_value=fake_beyonce, create=True):
            with mock.patch.object(helpers, "_sleep_ms"), mock.patch.object(helpers, "_sleep_sim_ms"), \
                 mock.patch.object(helpers, "_ensure_warp_signal_tracker"):
                res_dock = hook._execute_action(
                    fake_sm,
                    {"type": "dock", "target": "60003760", "item_id": 60003760, "wait_ms": 0, "reuse_pre_state": False, "scenario_mode": True, "time_control": False},
                )
                self.assertTrue(res_dock.get("ok"), res_dock)
                self.assertEqual(tc_calls, [])

                sess.stationid = None
                sess.solarsystemid = 30000142
                sess.solarsystemid2 = 30000142
                res_gate = hook._execute_action(
                    fake_sm,
                    {"type": "jump_gate", "target": "30001363", "item_id": 50001234, "wait_ms": 0, "reuse_pre_state": False, "scenario_mode": True, "time_control": False},
                )
                self.assertTrue(res_gate.get("ok"), res_gate)
                self.assertEqual(res_gate.get("to_system_id"), 30001363)
                self.assertEqual(tc_calls, [])

    def test_select_character_ensures_inflight_when_undocked_and_hangar_when_docked(self):
        hook = _load_hook_with_py2_stubs()
        ensured_views = []
        sess = types.SimpleNamespace(userid=1, charid=None, stationid=None, structureid=None, solarsystemid=None, solarsystemid2=None, mutating=0, changing=None)

        def _fake_select_char(cid):
            sess.charid = cid

        fake_char_svc = types.SimpleNamespace(SelectCharacterID=_fake_select_char)
        fake_session_mgr = types.SimpleNamespace(PerformSessionChange=lambda _tag, fn, cid, *_a: fn(cid))
        fake_svcs = {"sessionMgr": fake_session_mgr, "charUnboundMgr": fake_char_svc}
        fake_sm = types.SimpleNamespace(
            services=fake_svcs,
            GetService=lambda n: fake_svcs.get(n),
            RemoteSvc=lambda n: fake_svcs[n],
        )
        snap_base = {"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}
        fake_uicore = types.SimpleNamespace(
            cmd=None, registry=None, desktop=None,
            layer=types.SimpleNamespace(charsel=types.SimpleNamespace(slots=[])),
        )
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_get_state_snapshot", return_value=snap_base), \
             mock.patch.object(hook, "_settle_after_action", return_value=0), \
             mock.patch.object(hook, "_inspect_charsel_state", return_value={"active": True, "characters": [{"character_id": 90000001, "name": "Demo Pilot"}]}), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook.H, "_sync_quasar_gateway", create=True), \
             mock.patch.object(hook.H, "_ensure_view", side_effect=lambda _sm, v: ensured_views.append(v), create=True):
            sess.solarsystemid = 30000142
            sess.solarsystemid2 = 30000142
            res_space = hook._execute_action(fake_sm, {"type": "select_character", "target": "90000001", "character_id": 90000001, "wait_ms": 0, "reuse_pre_state": False})
            self.assertTrue(res_space.get("ok"), res_space)

            sess.charid = None
            sess.solarsystemid = None
            sess.stationid = 60003760
            res_dock = hook._execute_action(fake_sm, {"type": "select_character", "target": "90000001", "character_id": 90000001, "wait_ms": 0, "reuse_pre_state": False})
            self.assertTrue(res_dock.get("ok"), res_dock)

        self.assertEqual(ensured_views, ["inflight", "hangar"])

    def test_show_info_click_top_tabs_and_snapshot_cache_fields(self):
        hook = _load_hook_with_py2_stubs()
        self.assertEqual(hook.get_open_window_cmd_map(), hook.cmd_map)

        shown_info_calls = []
        clicked_tabs_log = []
        fake_info_svc = types.SimpleNamespace(
            ShowInfo=lambda tid, itemID=None: shown_info_calls.append((tid, itemID)),
        )
        tab_a = types.SimpleNamespace(text="Description", label="Description", name="tab_desc", destroyed=False, display=True, OnClick=lambda *_a: clicked_tabs_log.append("Description"))
        tab_b = types.SimpleNamespace(text="Attributes", label="Attributes", name="tab_attr", destroyed=False, display=True, OnClick=lambda *_a: clicked_tabs_log.append("Attributes"))
        fake_tg = types.SimpleNamespace(tabs=[tab_a, tab_b], children=[], destroyed=False, display=True, SelectByIdx=lambda i: None)
        fake_info_win = types.SimpleNamespace(windowID="InfoWindow", name="InfoWindow", caption="Information", destroyed=False, display=True, children=[fake_tg])
        fake_reg = types.SimpleNamespace(GetWindows=lambda: [fake_info_win])
        fake_uicore = types.SimpleNamespace(
            cmd=None, registry=fake_reg, desktop=types.SimpleNamespace(width=1280, height=800), layer=None,
        )
        inv_hangar = types.SimpleNamespace(items={1: {"categoryID": 6, "stacksize": 1}, 2: {"categoryID": 4, "stacksize": 500}})
        inv_cargo = types.SimpleNamespace(items={3: {"categoryID": 8, "quantity": 120}})
        fake_inv_cache = types.SimpleNamespace(inventories={(60003760, 4): inv_hangar, (100000001, 5): inv_cargo})
        fake_jc_svc = types.SimpleNamespace(homeStationID=60003760, jumpClones=[{"cloneID": 1}, {"cloneID": 2}])
        fake_svcs = {"info": fake_info_svc, "invCache": fake_inv_cache, "jumpCloneSvc": fake_jc_svc}
        fake_sm = types.SimpleNamespace(services=fake_svcs, GetService=lambda n: fake_svcs.get(n))
        sess = types.SimpleNamespace(userid=1, charid=90000001, stationid=60003760, structureid=None, shipid=100000001, solarsystemid=None, solarsystemid2=30000142, mutating=0, changing=None)

        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_is_visible", return_value=True), \
             mock.patch.object(hook.H, "_sleep_ms", create=True):
            snap = hook._get_state_snapshot(fake_sm)
            self.assertEqual(snap["inventory_state"]["hangar_item_count"], 2)
            self.assertEqual(snap["inventory_state"]["hangar_total_quantity"], 501)
            self.assertEqual(snap["inventory_state"]["cargo_item_count"], 1)
            self.assertEqual(snap["inventory_state"]["cargo_total_quantity"], 120)
            self.assertEqual(snap["clone_state"]["home_station_id"], 60003760)
            self.assertEqual(snap["clone_state"]["jump_clone_count"], 2)

            with mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                 mock.patch.object(hook, "_install_warp_telemetry"), \
                 mock.patch.object(hook, "_get_state_snapshot", return_value=snap), \
                 mock.patch.object(hook, "_settle_after_action", return_value=0), \
                 mock.patch.object(hook, "_invoke_widget_click", side_effect=lambda w, **_kw: w.OnClick()):
                res_si = hook._execute_action(fake_sm, {"type": "show_info", "type_id": 587, "category": "ship", "click_top_tabs": True, "wait_ms": 0, "reuse_pre_state": False})
                self.assertTrue(res_si.get("ok"), res_si)
                self.assertEqual(shown_info_calls, [(587, None)])
                self.assertEqual(res_si["tabs"]["tabs_clicked"], 2)
                self.assertEqual(clicked_tabs_log, ["Description", "Attributes"])

    def test_inventory_state_filters_cargo_flag_5_and_tracks_hangar_containers(self):
        hook = _load_hook_with_py2_stubs()
        fake_sess = types.SimpleNamespace(
            userid=1, charid=90000001, stationid=60003760, structureid=None,
            solarsystemid=None, solarsystemid2=30000142, shipid=100000001,
        )
        # Unified (ship_id, None) contains fitted HiSlot0 (flag 27), DroneBay (flag 87), and Cargo (flag 5).
        # Separate (ship_id, 87) DroneBay hold must NEVER pollute cargo_item_count.
        unified_ship = types.SimpleNamespace(items={
            201: {"itemID": 201, "flagID": 27, "categoryID": 7, "stacksize": 1},
            202: {"itemID": 202, "flagID": 87, "categoryID": 18, "stacksize": 5},
            301: {"itemID": 301, "flagID": 5, "categoryID": 8, "stacksize": 40},
        })
        drone_hold = types.SimpleNamespace(items={
            202: {"itemID": 202, "flagID": 87, "categoryID": 18, "stacksize": 5},
        })
        station_items_10004 = types.SimpleNamespace(items={
            401: {"itemID": 401, "flagID": 4, "categoryID": 4, "stacksize": 500},
        })
        fake_inv_cache = types.SimpleNamespace(inventories={
            (100000001, None): unified_ship,
            (100000001, 87): drone_hold,
            (10004, 4): station_items_10004,
        })
        fake_sm = types.SimpleNamespace(
            services={"invCache": fake_inv_cache},
            GetService=lambda n: fake_sm.services.get(n),
        )
        with mock.patch.dict(builtins.__dict__, {"session": fake_sess, "sm": fake_sm}), \
             mock.patch.object(hook, "_ensure_windowed_mode"), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_inspect_uicore_tree", return_value={"open_windows": [], "modals": []}):
            snap = hook._get_state_snapshot(fake_sm)
            inv = snap["inventory_state"]
            self.assertEqual(inv["cargo_item_count"], 1)
            self.assertEqual(inv["cargo_total_quantity"], 40)
            self.assertEqual(inv["hangar_containers_seen"], ["StationItems"])
            self.assertEqual(inv["hangar_non_ship_count"], 1)
            self.assertEqual(inv["hangar_non_ship_total_quantity"], 500)
            self.assertEqual(inv["ship_hangar_count"], 0)

            # When explicit (ship_id, 5) and (station_id, 4) StationHangar are also cached,
            # (ship_id, 5) stays authoritative and StationShips separates cleanly.
            fake_inv_cache.inventories[(100000001, 5)] = types.SimpleNamespace(items={
                301: {"itemID": 301, "flagID": 5, "categoryID": 8, "stacksize": 40},
                302: {"itemID": 302, "flagID": 5, "categoryID": 8, "stacksize": 20},
            })
            fake_inv_cache.inventories[(60003760, 4)] = types.SimpleNamespace(items={
                501: {"itemID": 501, "flagID": 4, "categoryID": 6, "stacksize": 1},
            })
            snap2 = hook._get_state_snapshot(fake_sm)
            inv2 = snap2["inventory_state"]
            self.assertEqual(inv2["cargo_item_count"], 2)
            self.assertEqual(inv2["cargo_total_quantity"], 60)
            self.assertEqual(inv2["hangar_containers_seen"], ["StationHangar", "StationItems"])
            self.assertEqual(inv2["hangar_non_ship_count"], 1)
            self.assertEqual(inv2["ship_hangar_count"], 1)
            self.assertEqual(inv2["hangar_item_count"], 2)
            self.assertEqual(inv2["hangar_total_quantity"], 501)

    def test_window_open_and_show_info_fail_when_expected_window_missing(self):
        hook = _load_hook_with_py2_stubs()
        fake_info_svc = types.SimpleNamespace(ShowInfo=lambda *_a, **_kw: None)
        fake_svcs = {"info": fake_info_svc}
        fake_sm = types.SimpleNamespace(services=fake_svcs, GetService=lambda n: fake_svcs.get(n))
        fake_cmd = types.SimpleNamespace(CmdOpenFleetFinderWindow=lambda: None)
        unrelated_win = types.SimpleNamespace(windowID="WalletWindow", name="WalletWindow", caption="Wallet", destroyed=False, display=True, children=[])
        fake_reg = types.SimpleNamespace(GetWindows=lambda: [unrelated_win])
        fake_uicore = types.SimpleNamespace(
            cmd=fake_cmd, registry=fake_reg, desktop=types.SimpleNamespace(width=1280, height=800), layer=None,
        )
        sess = types.SimpleNamespace(userid=1, charid=90000001, stationid=60003760, structureid=None, shipid=100000001, solarsystemid=None, solarsystemid2=30000142, mutating=0, changing=None)
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_is_visible", return_value=True), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"session": {"charid": 90000001}, "open_windows": []}), \
             mock.patch.object(hook, "_settle_after_action", return_value=0), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook.H, "_sleep_ms", create=True):
            res_ow = hook._execute_action(fake_sm, {
                "type": "open_window", "target": "fleet_finder", "require_window_open": True, "wait_ms": 0, "reuse_pre_state": False,
            })
            self.assertFalse(res_ow.get("action_ok"))
            self.assertFalse(res_ow.get("ok"))

            res_si = hook._execute_action(fake_sm, {
                "type": "show_info", "type_id": 587, "category": "ship", "wait_ms": 0, "reuse_pre_state": False,
            })
            self.assertFalse(res_si.get("action_ok"))
            self.assertFalse(res_si.get("ok"))

            res_tabs = hook._click_top_tabs_in_window("info", max_tabs=3, require_match=True)
            self.assertFalse(res_tabs.get("ok"))

    def test_window_sweep_chrome_protection_secondary_views_and_require_open(self):
        hook = _load_hook_with_py2_stubs()
        closed_wins = []

        def _make_win(wid, name=None, caption=None):
            w = types.SimpleNamespace(
                windowID=wid,
                name=name or wid,
                caption=caption or wid,
                destroyed=False,
                display=True,
                children=[],
            )
            def _close_win(win=w, win_id=wid):
                win.destroyed = True
                closed_wins.append(win_id)
            w.Close = _close_win
            return w

        lobby = _make_win("lobbyWnd", "lobbyWnd", "Station")
        neocom = _make_win("neocom", "neocom", "Neocom")
        sidepanel = _make_win("sidepanel", "sidepanel", "InfoPanel")
        chat = _make_win("chatchannel_local", "chatchannel_local", "Local")
        overview = _make_win("OverviewWindow", "OverviewWindow", "Overview")
        shipui = _make_win("ShipUI", "ShipUI", "ShipUI")
        infopanel = _make_win("InfoPanelLocations", "InfoPanelLocations", "Route")
        transient = _make_win("WalletWindow", "WalletWindow", "Wallet")
        windows = [lobby, neocom, sidepanel, chat, overview, shipui, infopanel, transient]

        fake_reg = types.SimpleNamespace(
            GetWindows=lambda: list(windows),
            GetModalWindow=lambda: None,
            modals=[],
        )
        fake_cmd = types.SimpleNamespace(
            CmdOpenNewEdenStore=lambda: None,
            CmdOpenSkillQueueWindow=lambda: None,
            CmdToggleShipTree=lambda: None,
        )
        fake_uicore = types.SimpleNamespace(
            cmd=fake_cmd,
            registry=fake_reg,
            layer=types.SimpleNamespace(modal=None, abovemain=None, main=None),
            desktop=types.SimpleNamespace(width=1280, height=800),
        )
        cur_view_state = {
            "secondary": types.SimpleNamespace(name="starmap_new"),
            "current": types.SimpleNamespace(name="starmap_new"),
        }
        fake_viewstate = types.SimpleNamespace(
            secondaryInfo=cur_view_state["secondary"],
            activeView=types.SimpleNamespace(
                layer=types.SimpleNamespace(name="l_starmap", display=True, destroyed=False, children=[])
            ),
            GetCurrentViewInfo=lambda: cur_view_state["current"],
            ToggleSecondaryView=lambda *_a, **_kw: None,
            CloseSecondaryView=lambda *_a, **_kw: None,
        )
        fake_notif_center = types.SimpleNamespace(
            isShowingDrawer=False,
            drawer=types.SimpleNamespace(name="NotificationCenterDrawer", display=False, destroyed=True, children=[]),
            OpenDrawer=lambda: None,
            CloseDrawer=lambda: None,
        )
        fake_skill_panel_cls = type(
            "SkillPlanDockablePanel",
            (),
            {"Open": staticmethod(lambda: None), "GetIfOpen": staticmethod(lambda: None)},
        )
        fake_skill_panel_mod = types.ModuleType("eve.client.script.ui.skillPlan.skillPlanDockablePanel")
        fake_skill_panel_mod.SkillPlanDockablePanel = fake_skill_panel_cls
        fake_eve_pkg = types.ModuleType("eve")
        fake_client_pkg = types.ModuleType("eve.client")
        fake_script_pkg = types.ModuleType("eve.client.script")
        fake_ui_pkg = types.ModuleType("eve.client.script.ui")
        fake_sp_pkg = types.ModuleType("eve.client.script.ui.skillPlan")
        fake_svcs = {
            "viewState": fake_viewstate,
            "notificationUIService": types.SimpleNamespace(notificationCenter=fake_notif_center),
            "notificationSvc": types.SimpleNamespace(
                GetAllFormattedNotifications=lambda: [],
                OpenNotificationReadingWnd=lambda: None,
            ),
            "skillPlanSvc": types.SimpleNamespace(GetAllPersonal=lambda: []),
            "aurumstore": types.SimpleNamespace(),
            "skills": types.SimpleNamespace(),
        }
        fake_sm = types.SimpleNamespace(services=fake_svcs, GetService=lambda n: fake_svcs.get(n))
        sess = types.SimpleNamespace(
            userid=1, charid=90000001, stationid=60003760, structureid=None,
            shipid=100000001, solarsystemid=None, solarsystemid2=30000142, mutating=0, changing=None,
        )
        with mock.patch.dict(
            sys.modules,
            {
                "uthread": None,
                "eve": fake_eve_pkg,
                "eve.client": fake_client_pkg,
                "eve.client.script": fake_script_pkg,
                "eve.client.script.ui": fake_ui_pkg,
                "eve.client.script.ui.skillPlan": fake_sp_pkg,
                "eve.client.script.ui.skillPlan.skillPlanDockablePanel": fake_skill_panel_mod,
            },
        ), \
             mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_is_visible", side_effect=lambda obj: bool(getattr(obj, "display", True) and not getattr(obj, "destroyed", False))), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"session": {"charid": 90000001}, "open_windows": []}), \
             mock.patch.object(hook, "_settle_after_action", return_value=0), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook.H, "_sleep_ms", create=True):
            # Generic close (target="") closes transient WalletWindow, preserving all HUD/lobby/chat/overview/infopanel
            res_empty = hook._execute_action(fake_sm, {"type": "close_modal_or_window", "target": "", "wait_ms": 0})
            self.assertTrue(res_empty.get("action_ok"), res_empty)
            self.assertEqual(closed_wins, ["WalletWindow"])

            # Target "info" must NOT close InfoPanelLocations when no InfoWnd is open
            closed_wins[:] = []
            res_info = hook._execute_action(fake_sm, {"type": "close_modal_or_window", "target": "info", "wait_ms": 0})
            self.assertTrue(res_info.get("action_ok"), res_info)
            self.assertEqual(closed_wins, [])

            # Secondary view without TabGroup ("map" -> currentSecondaryView="starmap_new") succeeds gracefully with tabs_clicked=0
            tab_map = hook._click_top_tabs_in_window("map", max_tabs=4, require_match=True)
            self.assertTrue(tab_map.get("ok"), tab_map)
            self.assertEqual(tab_map.get("tabs_clicked"), 0)

            # Secondary view NOT open ("shiptree" while "starmap_new" is open) must fail when require_match=True
            tab_tree = hook._click_top_tabs_in_window("shiptree", max_tabs=4, require_match=True)
            self.assertFalse(tab_tree.get("ok"), tab_tree)

            # With unrelated secondary view ("starmap_new") already open, no-op opens must still fail closed under require_open=True
            res_skills_sec = hook._open_client_window(fake_sm, "skills", require_open=True)
            self.assertFalse(res_skills_sec.get("ok"), res_skills_sec)
            res_notif_sec = hook._open_client_window(fake_sm, "notifications", require_open=True)
            self.assertFalse(res_notif_sec.get("ok"), res_notif_sec)
            res_tree_sec = hook._open_client_window(fake_sm, "shiptree", require_open=True)
            self.assertFalse(res_tree_sec.get("ok"), res_tree_sec)
            fake_uicore.cmd = types.SimpleNamespace()  # force aurumstore fallback
            res_nes_sec = hook._open_client_window(fake_sm, "nes", require_open=True)
            self.assertFalse(res_nes_sec.get("ok"), res_nes_sec)

            # With only primary view ("hangar") and secondaryInfo=None, no-op opens must never report ok via GetCurrentViewInfo()=="hangar"
            fake_uicore.cmd = fake_cmd
            fake_viewstate.secondaryInfo = None
            cur_view_state["current"] = types.SimpleNamespace(name="hangar")
            res_skills_pri = hook._open_client_window(fake_sm, "skills", require_open=True)
            self.assertFalse(res_skills_pri.get("ok"), res_skills_pri)
            res_notif_pri = hook._open_client_window(fake_sm, "notifications", require_open=True)
            self.assertFalse(res_notif_pri.get("ok"), res_notif_pri)
            res_tree_pri = hook._open_client_window(fake_sm, "shiptree", require_open=True)
            self.assertFalse(res_tree_pri.get("ok"), res_tree_pri)
            fake_uicore.cmd = types.SimpleNamespace()
            res_nes_pri = hook._open_client_window(fake_sm, "nes", require_open=True)
            self.assertFalse(res_nes_pri.get("ok"), res_nes_pri)


class TestComboDropdownAndFormWndLayout(unittest.TestCase):
    @staticmethod
    def _build_v24_combo_and_menu_layer():
        import weakref

        class FakeContainer:
            def __init__(self, name="", parent=None):
                self.name = name
                self.parent = parent
                self.children = []
                self.destroyed = False
                self.display = True
                self.closed_calls = 0
                self.sr = types.SimpleNamespace()
                if parent is not None:
                    parent.children.append(self)

            def Close(self):
                self.closed_calls += 1
                self.destroyed = True
                for sub in list(self.children):
                    if hasattr(sub, "Close"):
                        sub.Close()
                if self.parent is not None and self in self.parent.children:
                    self.parent.children.remove(self)

            def Flush(self):
                for sub in list(self.children):
                    sub.Close()

        menu_layer = FakeContainer(name="l_menu")

        class FakeCombo:
            def __init__(self, name="startStation", parent=None):
                self.name = name
                self.parent = parent
                self.children = []
                self.destroyed = False
                self.display = True
                self._comboDropDown = None
                self.optionMenu = None
                self.expand_calls = 0
                self.cleanup_calls = []
                self.collapse_calls = 0
                self.onchange_calls = []
                self.bg_expanded = False
                if parent is not None:
                    parent.children.append(self)

            def _Expanded(self):
                return bool(self._comboDropDown and self._comboDropDown())

            def Expand(self):
                self.expand_calls += 1
                menu = FakeContainer(name="comboOptionMenu", parent=menu_layer)
                scroll = FakeContainer(name="comboScroll", parent=menu)
                scroll.OnKillFocus = self.OnScrollFocusLost
                scroll.Confirm = self.Confirm
                scroll.OnUp = self.OnUp
                scroll.OnDown = self.OnDown
                scroll.OnSelectionChange = self.OnScrollSelectionChange
                entry = FakeContainer(name="ComboEntry_0", parent=scroll)
                entry.OnClick = lambda: self.Confirm()
                menu.sr.scroll = scroll
                self.optionMenu = menu
                self._comboDropDown = weakref.ref(menu)
                self.bg_expanded = True

            def OnClick(self):
                if not self._Expanded():
                    self.Expand()

            def OnScrollFocusLost(self, *args):
                self.Confirm(False)

            def Confirm(self, setfocus=True):
                self.onchange_calls.append("confirmed")
                self.Cleanup(setfocus)

            def OnUp(self):
                pass

            def OnDown(self):
                pass

            def OnScrollSelectionChange(self, *args):
                pass

            def CollapseBackground(self):
                self.collapse_calls += 1
                self.bg_expanded = False

            def Cleanup(self, setfocus=True):
                self.cleanup_calls.append(bool(setfocus))
                menu_layer.Flush()
                self._comboDropDown = None
                self.CollapseBackground()

        return menu_layer, FakeContainer, FakeCombo

    @staticmethod
    def _build_fake_carbon_menu_mods():
        class ContextMenuMixin:
            pass

        def CloseContextMenus():
            # V24 CloseContextMenus only closes ContextMenuMixin widgets, not Combo optionMenu containers.
            return None

        carbonui_pkg = types.ModuleType("carbonui")
        control_pkg = types.ModuleType("carbonui.control")
        ctx_pkg = types.ModuleType("carbonui.control.contextMenu")
        menu_util_mod = types.ModuleType("carbonui.control.contextMenu.menuUtil")
        menu_util_mod.CloseContextMenus = CloseContextMenus
        ctx_mod = types.ModuleType("carbonui.control.contextMenu.contextMenu")
        ctx_mod.ContextMenuMixin = ContextMenuMixin
        return {
            "carbonui": carbonui_pkg,
            "carbonui.control": control_pkg,
            "carbonui.control.contextMenu": ctx_pkg,
            "carbonui.control.contextMenu.menuUtil": menu_util_mod,
            "carbonui.control.contextMenu.contextMenu": ctx_mod,
        }

    def test_close_context_menus_if_outside_cleans_up_combo_dropdown_and_allows_reopen(self):
        # Fixes #1289
        hook = _load_hook_with_py2_stubs()
        menu_layer, _, FakeCombo = self._build_v24_combo_and_menu_layer()
        combo = FakeCombo("startStation")
        fake_uicore = types.SimpleNamespace(
            layer=types.SimpleNamespace(menu=menu_layer),
            registry=None,
            uilib=None,
            cmd=None,
            desktop=None,
        )

        with mock.patch.dict(sys.modules, self._build_fake_carbon_menu_mods()), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False):
            res_open_1 = hook._invoke_widget_click(combo)
            self.assertEqual(res_open_1, "OnClick")
            self.assertEqual(combo.expand_calls, 1)
            self.assertTrue(combo._Expanded())
            self.assertTrue(combo.bg_expanded)
            self.assertEqual(len(menu_layer.children), 1)

            # Clicking an entry inside optionMenu must keep optionMenu open for selection.
            entry = menu_layer.children[0].sr.scroll.children[0]
            hook._close_context_menus_if_outside(entry)
            self.assertTrue(combo._Expanded())
            self.assertEqual(len(menu_layer.children), 1)

            # External dismissal (e.g., open_window / open_neocom -> _close_context_menus_if_outside(None))
            # must run Combo.Cleanup(False) instead of raw ch.Close(), without firing OnChange.
            first_menu = combo.optionMenu
            hook._close_context_menus_if_outside(None)
            self.assertTrue(first_menu.destroyed)
            self.assertEqual(menu_layer.children, [])
            self.assertEqual(combo.cleanup_calls, [False])
            self.assertEqual(combo.onchange_calls, [])
            self.assertIsNone(combo._comboDropDown)
            self.assertFalse(combo._Expanded())
            self.assertFalse(combo.bg_expanded)
            self.assertEqual(combo.collapse_calls, 1)

            # Re-clicking startStation Combo must now re-expand instead of silently no-oping.
            res_open_2 = hook._invoke_widget_click(combo)
            self.assertEqual(res_open_2, "OnClick")
            self.assertEqual(combo.expand_calls, 2)
            self.assertTrue(combo._Expanded())
            self.assertEqual(len(menu_layer.children), 1)

    def test_close_context_menus_preserves_pinned_tooltip_and_resets_stale_combo_dropdown(self):
        # Fixes #1289
        hook = _load_hook_with_py2_stubs()
        menu_layer, FakeContainer, FakeCombo = self._build_v24_combo_and_menu_layer()
        combo = FakeCombo("startStation")
        outside_btn = FakeContainer("otherBtn")
        fake_uicore = types.SimpleNamespace(
            layer=types.SimpleNamespace(menu=menu_layer),
            registry=None,
            uilib=None,
            cmd=None,
            desktop=None,
        )

        with mock.patch.dict(sys.modules, self._build_fake_carbon_menu_mods()), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False):
            combo.Expand()
            pinned_tp = FakeContainer("TooltipPanel", parent=menu_layer)
            pinned_tp._evomu_pinned = True

            # Outside click with a pinned tooltip sibling must preserve pinned_tp (not Flush menu_layer)
            # while still closing combo's optionMenu and resetting _comboDropDown / background state.
            hook._close_context_menus_if_outside(outside_btn)
            self.assertFalse(pinned_tp.destroyed)
            self.assertEqual(pinned_tp.closed_calls, 0)
            self.assertTrue(combo.optionMenu.destroyed)
            self.assertEqual(combo.cleanup_calls, [])
            self.assertIsNone(combo._comboDropDown)
            self.assertFalse(combo._Expanded())
            self.assertFalse(combo.bg_expanded)
            self.assertEqual(combo.collapse_calls, 1)

            # Simulate an already-wedged Combo (e.g. optionMenu destroyed externally while
            # self.optionMenu + weakref _comboDropDown still keep _Expanded() == True).
            combo.Expand()
            stale_menu = combo.optionMenu
            stale_menu.Close()
            self.assertTrue(stale_menu.destroyed)
            self.assertTrue(combo._Expanded())

            res_reopen = hook._invoke_widget_click(combo)
            self.assertEqual(res_reopen, "OnClick")
            self.assertEqual(combo.expand_calls, 3)
            self.assertTrue(combo._Expanded())
            self.assertEqual(combo.collapse_calls, 2)

    def test_invoke_widget_click_and_drag_widget_skip_formwnd_and_target_parent_window(self):
        # Fixes #1289
        hook = _load_hook_with_py2_stubs()

        class FakeRectWidget:
            def __init__(self, name, cls_name, x, y, w, h, parent=None):
                self.name = name
                self.__class__ = type(cls_name, (FakeRectWidget,), {})
                self._rect = (x, y, w, h)
                self.left = x
                self.top = y
                self.parent = parent
                self.children = []
                self.destroyed = False
                self.display = True
                self.state = 0
                self.set_order_calls = []
                if parent is not None:
                    parent.children.append(self)

            def GetAbsolute(self):
                return self._rect

            def SetOrder(self, idx):
                self.set_order_calls.append(idx)

        main_layer = FakeRectWidget("l_main", "Layer", 0, 0, 1920, 1080)
        create_contract_wnd = FakeRectWidget("createcontract", "CreateContractWnd", 700, 150, 740, 720, parent=main_layer)
        create_contract_wnd.windowID = "createcontract"
        create_contract_wnd.isTopLevelWindow = True

        sr_main = FakeRectWidget("main", "Container", 700, 180, 740, 690, parent=create_contract_wnd)
        FakeRectWidget("topParent", "Container", 700, 180, 740, 42, parent=sr_main)
        FakeRectWidget("buttonWnd", "ButtonGroup", 700, 830, 740, 40, parent=sr_main)
        form_wnd = FakeRectWidget("form", "FormWnd", 700, 222, 740, 608, parent=sr_main)
        start_station = FakeRectWidget("startStation", "Combo", 740, 538, 380, 32, parent=form_wnd)
        combo_clicks = []
        start_station.OnClick = lambda: combo_clicks.append("clicked")
        drag_handle = FakeRectWidget("headerLabel", "Label", 720, 240, 180, 24, parent=form_wnd)

        # Also verify a FormWnd inside a mock window without windowID/isTopLevelWindow is skipped.
        custom_wnd = FakeRectWidget("custom", "MyCustomWnd", 100, 100, 400, 300, parent=main_layer)
        custom_form = FakeRectWidget("form", "FormWnd", 100, 130, 400, 270, parent=custom_wnd)
        custom_btn = FakeRectWidget("ok_btn", "Button", 120, 150, 80, 28, parent=custom_form)
        custom_btn.OnClick = lambda: combo_clicks.append("custom_ok")

        focused = []
        fake_reg = types.SimpleNamespace(SetFocus=lambda obj: focused.append(obj))
        fake_desktop = types.SimpleNamespace(width=1920, height=1080)
        fake_uicore = types.SimpleNamespace(
            layer=types.SimpleNamespace(menu=None, main=main_layer, abovemain=None, modal=None),
            registry=fake_reg,
            uilib=None,
            cmd=None,
            desktop=fake_desktop,
        )
        fake_sm = mock.MagicMock()

        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
             mock.patch.object(hook, "_find_widget", return_value=drag_handle), \
             mock.patch.object(hook, "_find_widget_at_xy", return_value=None):
            res_combo = hook._invoke_widget_click(start_station)
            self.assertEqual(res_combo, "OnClick")
            self.assertEqual(form_wnd.set_order_calls, [])
            self.assertEqual(create_contract_wnd.set_order_calls, [0])
            self.assertEqual(focused, [create_contract_wnd])

            res_custom = hook._invoke_widget_click(custom_btn)
            self.assertEqual(res_custom, "OnClick")
            self.assertEqual(custom_form.set_order_calls, [])
            self.assertEqual(custom_wnd.set_order_calls, [0])
            self.assertEqual(combo_clicks, ["clicked", "custom_ok"])

            res_drag = hook._execute_action(
                fake_sm,
                {"type": "drag_widget", "target": "headerLabel", "dest_x": 920, "dest_y": 340, "wait_ms": 0},
            )
            self.assertTrue(res_drag.get("ok"), res_drag)
            self.assertEqual(res_drag.get("moved_window"), "createcontract")
            self.assertEqual((form_wnd.left, form_wnd.top), (700, 222))
            self.assertNotEqual((create_contract_wnd.left, create_contract_wnd.top), (700, 150))

    def test_item_entry_double_click_docked_structure_falls_back_to_showinfo(self):
        hook = _load_hook_with_py2_stubs()
        shown_calls = []
        windows = []

        def _fake_show_info(tid, itemID=None):
            shown_calls.append((tid, itemID))
            windows.append(
                types.SimpleNamespace(
                    windowID="InfoWindow",
                    name="InfoWindow",
                    caption="Keepstar - Information",
                    destroyed=False,
                    display=True,
                )
            )

        info_svc = types.SimpleNamespace(ShowInfo=_fake_show_info)
        svcs = {"info": info_svc}
        fake_sm = types.SimpleNamespace(services=svcs, GetService=lambda name: svcs.get(name))
        docked_sess = types.SimpleNamespace(
            stationid=60003760,
            structureid=None,
            solarsystemid=None,
            shipid=100000001,
        )
        fake_reg = types.SimpleNamespace(GetWindows=lambda: list(windows))
        fake_uicore = types.SimpleNamespace(
            layer=types.SimpleNamespace(menu=None, main=None, abovemain=None, modal=None),
            registry=fake_reg,
            uilib=None,
            cmd=None,
            desktop=types.SimpleNamespace(width=1280, height=800),
        )

        class InvItem:
            pass

        entry = InvItem()
        entry.name = "ItemEntry_35834"
        entry.children = []
        entry.destroyed = False
        entry.display = True
        entry.isContainer = False
        rec = types.SimpleNamespace(typeID=35834, itemID=100001234, categoryID=65, groupID=1657)
        scroll = mock.Mock()
        sr_node = types.SimpleNamespace(scroll=scroll, selected=False, itemID=100001234, typeID=35834, rec=rec)
        entry.sr = types.SimpleNamespace(node=sr_node)
        entry.rec = rec
        # Mimic EVE InvItem.OnDblClick -> structureDeployment.Deploy silent no-op while docked
        entry.OnMouseDown = lambda: None
        entry.OnDblClick = lambda: None

        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.dict(builtins.__dict__, {"session": docked_sess, "sm": fake_sm}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_is_visible", return_value=True), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False), \
             mock.patch.object(hook, "_resolve_editable_widget", return_value=None), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_settle_after_action", return_value=0), \
             mock.patch.object(hook, "_find_widget", return_value=entry), \
             mock.patch.object(hook.H, "_sleep_ms", create=True):
            res = hook._execute_action(
                fake_sm,
                {"type": "click_widget", "target": "ItemEntry_35834", "double_click": True, "wait_ms": 0},
            )

        self.assertTrue(res.get("ok"), res)
        self.assertEqual(res.get("clicked_widget"), "OnDblClick->ShowInfo")
        self.assertEqual(shown_calls, [(35834, 100001234)])
        self.assertTrue(
            any("InfoWindow" in w for w in ((res.get("state_diff") or {}).get("windows_opened") or [])),
            res,
        )

    def test_item_entry_double_click_preserves_ship_in_space_deploy_already_opened_and_user_error(self):
        hook = _load_hook_with_py2_stubs()
        shown_calls = []
        windows = []

        info_svc = types.SimpleNamespace(ShowInfo=lambda tid, itemID=None: shown_calls.append((tid, itemID)))
        svcs = {"info": info_svc}
        fake_sm = types.SimpleNamespace(services=svcs, GetService=lambda name: svcs.get(name))
        fake_reg = types.SimpleNamespace(GetWindows=lambda: list(windows))
        fake_uicore = types.SimpleNamespace(
            layer=types.SimpleNamespace(menu=None, main=None, abovemain=None, modal=None),
            registry=fake_reg,
            uilib=None,
            cmd=None,
            desktop=types.SimpleNamespace(width=1280, height=800),
        )

        class InvItem:
            pass

        def _make_entry(name, tid, iid, cat_id, group_id, on_dbl, is_container=False):
            e = InvItem()
            e.name = name
            e.children = []
            e.destroyed = False
            e.display = True
            e.isContainer = is_container
            rec = types.SimpleNamespace(typeID=tid, itemID=iid, categoryID=cat_id, groupID=group_id)
            sr_node = types.SimpleNamespace(scroll=None, selected=False, itemID=iid, typeID=tid, rec=rec, isContainer=is_container)
            e.sr = types.SimpleNamespace(node=sr_node)
            e.rec = rec
            e.OnMouseDown = lambda: None
            e.OnDblClick = on_dbl
            return e

        class UserError(Exception):
            def __init__(self, msg):
                super().__init__(msg)
                self.msg = msg

        in_space_sess = types.SimpleNamespace(stationid=None, structureid=None, solarsystemid=30000142, shipid=100000001)
        docked_sess = types.SimpleNamespace(stationid=60003760, structureid=None, solarsystemid=None, shipid=100000001)

        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_is_visible", return_value=True), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False), \
             mock.patch.object(hook, "_resolve_editable_widget", return_value=None), \
             mock.patch.object(hook.H, "_sleep_ms", create=True):
            # 1) Structure in space: preserve OnDblClick (do not intercept StructureDeployment.Deploy)
            in_space_struct = _make_entry("ItemEntry_35834", 35834, 2001, 65, 1657, lambda: None)
            with mock.patch.dict(builtins.__dict__, {"session": in_space_sess, "sm": fake_sm}):
                self.assertEqual(hook._invoke_widget_click(in_space_struct, double_click=True), "OnDblClick")

            # 2) Docked ship & container: never fall back to ShowInfo
            ship_entry = _make_entry("ItemEntry_34828", 34828, 2002, 6, 25, lambda: None)
            cont_entry = _make_entry("ItemEntry_3293", 3293, 2003, 2, 12, lambda: None, is_container=True)
            with mock.patch.dict(builtins.__dict__, {"session": docked_sess, "sm": fake_sm}):
                self.assertEqual(hook._invoke_widget_click(ship_entry, double_click=True), "OnDblClick")
                self.assertEqual(hook._invoke_widget_click(cont_entry, double_click=True), "OnDblClick")

            # 3) Normal item whose OnDblClick opens InfoWindow itself: do not call ShowInfo a second time
            def _open_info_directly():
                windows.append(
                    types.SimpleNamespace(
                        windowID="InfoWindow",
                        name="InfoWindow",
                        caption="Tritanium - Information",
                        destroyed=False,
                        display=True,
                    )
                )

            mineral_entry = _make_entry("ItemEntry_34", 34, 2004, 4, 18, _open_info_directly)
            with mock.patch.dict(builtins.__dict__, {"session": docked_sess, "sm": fake_sm}):
                self.assertEqual(hook._invoke_widget_click(mineral_entry, double_click=True), "OnDblClick")

            # 4) UserError inside sr.node OnDblClick is caught cleanly and formatted
            def _raise_user_err():
                raise UserError("CantDeployBlocked")

            err_entry = _make_entry("ItemEntry_35834", 35834, 2005, 65, 1657, _raise_user_err)
            with mock.patch.dict(builtins.__dict__, {"session": in_space_sess, "sm": fake_sm}), \
                 mock.patch.object(hook, "_notify_user_error") as notify_mock:
                out_err = hook._invoke_widget_click(err_entry, double_click=True)
                self.assertEqual(out_err, "UserError(CantDeployBlocked)")
                notify_mock.assert_called_once()

        self.assertEqual(shown_calls, [])


if __name__ == "__main__":
    unittest.main()
