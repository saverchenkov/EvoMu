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
    fake_helpers._advance_clock = lambda *_args, **_kwargs: False
    fake_helpers._clear_session_timer = lambda *_args, **_kwargs: False
    fake_helpers._wait_session_timer_clear = lambda max_wait_sec=12.0: None
    fake_helpers._try_slash = lambda *_args, **_kwargs: None
    fake_helpers._configure_host_gm_bridge = lambda *_args, **_kwargs: None
    fake_helpers.snapshot_sim_time_cursor = lambda: 0
    fake_helpers.collect_sim_time_telemetry = lambda _since=0: {
        "sim_time_injected_sec": 0.0,
        "sim_time_injection_count": 0,
        "sim_time_injections": [],
        "sim_seq_after": 0,
    }
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
        w_chat_stack = FakeWindow("ChatWindowStack", "ChatWindowStack", "ChatWindowStack")
        w_paint = FakeWindow("PaintTool", "PaintTool", "PaintToolWnd")
        w_agency = FakeWindow("AgencyWndNew", "AgencyWndNew", "AgencyWndNew")
        w_stack = FakeStackedWindow("InventorySpace", "InventorySpace", "InventoryPrimary")

        class FakeReg:
            def GetWindows(self):
                return [w_modal, w_lobby, w_chat, w_chat_stack, w_paint, w_agency, w_stack]

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
        self.assertFalse(w_chat_stack.closed)


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

    def test_open_and_close_paint_tool_alias_targets_ship_skinr(self):
        hook = _load_hook_with_py2_stubs()
        view_state = {"name": "hangar"}
        cmd_calls = []
        closed_views = []

        def open_ship_skinr():
            cmd_calls.append("OpenShipSKINRWindow")
            view_state["name"] = "ship_skinr"

        def close_sec_view(name):
            closed_views.append(name)
            if name == "ship_skinr":
                view_state["name"] = "hangar"

        fake_vs = types.SimpleNamespace(
            GetCurrentViewInfo=lambda: types.SimpleNamespace(name=view_state["name"]),
            CloseSecondaryView=close_sec_view,
        )
        fake_sm = mock.Mock()
        fake_sm.services = {"viewState": fake_vs}
        fake_sm.GetService.side_effect = lambda name: fake_vs if name == "viewState" else mock.Mock()
        fake_cmd = types.SimpleNamespace(OpenShipSKINRWindow=open_ship_skinr)
        fake_uicore = types.SimpleNamespace(
            cmd=fake_cmd,
            registry=types.SimpleNamespace(GetWindows=lambda: []),
            layer=None,
            dockablePanelManager=None,
        )

        with mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_is_visible", return_value=True), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_state_snapshot", side_effect=lambda _sm=None: {
                 "open_windows": [],
                 "modals": [],
                 "ui": {},
                 "space": {},
                 "skills_state": {},
                 "wallet_state": {},
                 "active_view": view_state["name"],
             }), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore):
            open_res = hook._execute_action(
                fake_sm,
                {"type": "open_neocom", "target": "paint_tool", "require_window_open": True, "wait_ms": 0},
            )
            self.assertTrue(open_res.get("ok"), open_res)
            self.assertEqual(cmd_calls, ["OpenShipSKINRWindow"])
            self.assertEqual(view_state["name"], "ship_skinr")

            close_res = hook._execute_action(
                fake_sm,
                {"type": "close_window", "target": "paint_tool", "wait_ms": 0},
            )
            self.assertTrue(close_res.get("ok"), close_res)
            self.assertEqual(closed_views, ["ship_skinr"])
            self.assertEqual(view_state["name"], "hangar")

            open_ship_skinr()
            cleanup_res = hook._cleanup_utility_windows(fake_sm)
            self.assertEqual(cleanup_res.get("closed_secondary_view"), "ship_skinr")
            self.assertEqual(view_state["name"], "hangar")

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

        def _fake_advance_clock(_sm, _ipc, secs, **_kwargs):
            advance_calls.append(int(secs))
            dist_state["sd"] = max(0.0, dist_state["sd"] - float(secs) * 1000.0)
            if dist_state["sd"] <= 2000.0:
                fake_sess.stationid = 60000685
            return True

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
            "stationSvc": FakeStationSvc(),
        }[name]
        fake_es = types.SimpleNamespace(_EXC_SEQ=0, _RECENT_EXCEPTIONS=[], _PT_CONSUMED_EXC_SEQ=0, _REPORTED_LOG_EXC_SIGS=set())

        with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
             mock.patch.object(builtins, "session", fake_sess, create=True), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_clear_session_timer", return_value=True, create=True), \
             mock.patch.object(hook.H, "_advance_clock", side_effect=_fake_advance_clock, create=True), \
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
             mock.patch.object(hook.H, "_clear_session_timer", return_value=True, create=True), \
             mock.patch.object(hook.H, "_advance_clock", side_effect=_fake_advance_clock, create=True), \
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

        def _fake_advance_clock(_sm, _ipc, secs, **_kwargs):
            advance_calls.append(int(secs))
            return True

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
            "stationSvc": FakeStationSvc(),
        }[name]
        fake_es = types.SimpleNamespace(_EXC_SEQ=0, _RECENT_EXCEPTIONS=[], _PT_CONSUMED_EXC_SEQ=0, _REPORTED_LOG_EXC_SIGS=set())

        with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
             mock.patch.object(builtins, "session", fake_sess, create=True), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_clear_session_timer", return_value=True, create=True), \
             mock.patch.object(hook.H, "_advance_clock", side_effect=_fake_advance_clock, create=True), \
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

    def test_close_window_blocked_by_confirm_modal_does_not_force_close_when_user_clicks_no(self):
        hook = _load_hook_with_py2_stubs()

        class DiscardModal:
            windowID = "message"
            name = "modal"
            isModal = True
            destroyed = False
            display = True

        class DraftMailWnd:
            windowID = "NewMessageWindow"
            name = "newmessage"
            caption = "New Message"
            destroyed = False
            display = True
            close_calls = 0

            def CloseByUser(self):
                # Pop confirmation modal and block until user dismisses with No
                active_windows.append(modal_wnd)
                while not modal_wnd.destroyed:
                    clock["t"] += 0.1
                # User chose No: DraftMailWnd stays alive
                return

            def Close(self):
                self.close_calls += 1
                self.destroyed = True

        draft_wnd = DraftMailWnd()
        modal_wnd = DiscardModal()
        active_windows = [draft_wnd]
        deferred_workers = []

        fake_registry = types.SimpleNamespace(GetWindows=lambda: list(active_windows))
        fake_uicore = types.SimpleNamespace(registry=fake_registry, layer=None, cmd=None)
        fake_sm = mock.MagicMock()

        def fake_uthread_new(fn):
            deferred_workers.append(fn)

        clock = {"t": 1000.0}

        def fake_time():
            return clock["t"]

        def fake_sleep_ms(ms):
            clock["t"] += float(ms) / 1000.0

        with mock.patch.dict(sys.modules, {"uthread": types.SimpleNamespace(new=fake_uthread_new)}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook, "_is_visible", side_effect=lambda w: bool(getattr(w, "display", True))), \
             mock.patch.object(hook.H, "_sleep_ms", side_effect=fake_sleep_ms, create=True), \
             mock.patch.object(hook.time, "time", side_effect=fake_time), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"):
            # Simulated interactive pump: first call advances past 0.8s modal block;
            # on first sleep after worker starts, spawn CloseByUser up to modal pop!
            def drive_worker_sleep(ms):
                fake_sleep_ms(ms)
                if modal_wnd not in active_windows:
                    active_windows.append(modal_wnd)

            with mock.patch.object(hook.H, "_sleep_ms", side_effect=drive_worker_sleep, create=True):
                res = hook._execute_action(fake_sm, {"type": "close_window", "target": "newmessage", "wait_ms": 250})

            self.assertTrue(res.get("ok"), res)
            self.assertTrue(res.get("blocked_by_modal"), res)
            self.assertEqual(len(deferred_workers), 1)

            # Now user clicks No: modal is dismissed BEFORE worker unwinds CloseByUser()
            modal_wnd.destroyed = True
            active_windows.remove(modal_wnd)
            deferred_workers[0]()

        self.assertEqual(draft_wnd.close_calls, 0, "blocked CloseByUser must not fall back to force Close() after user clicked No")
        self.assertFalse(draft_wnd.destroyed)


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

    def test_quasar_async_response_channel_flight_and_settle_extends_past_default_cap(self):
        import gc

        hook = _load_hook_with_py2_stubs()
        hook._FLIGHT_RECORDER = []
        hook._FLIGHT_SEQ = 0
        hook._QUASAR_IN_FLIGHT.clear()
        hook._QUASAR_IN_FLIGHT_SEQ = 0
        hook._MACHO_CALL_TASKLET_DEPTH.clear()

        class FakeDeleteRequest:
            pass

        class FakeRespChannel:
            def __init__(self, fail_status=None):
                self._open = True
                self._fail_status = fail_status

            def receive(self):
                self._open = False
                if self._fail_status is not None:
                    exc = RuntimeError("rpc error")
                    exc.response_primitive = types.SimpleNamespace(status_code=self._fail_status)
                    raise exc
                return ("resp_prim", "resp_payload")

        channels_in_flight = []

        class FakePublicGatewaySvc:
            def __init__(self):
                self.publicGateway = types.SimpleNamespace(
                    get_request_broker_details=lambda: {"pending_requests": 0, "pending_responses": 0}
                )

            def send_character_request(self, req_payload, expected_response_class=None, timeout_seconds=15):
                ch = FakeRespChannel()
                channels_in_flight.append(ch)
                return ("req_prim", ch)

            def send_user_request(self, req_payload, expected_response_class=None, timeout_seconds=15):
                return ("req_prim", FakeRespChannel(fail_status=404))

            def send_request(self, req_payload):
                return ("req_prim", FakeRespChannel())

            def send_blocking_character_request_and_receive_response(self, req_payload):
                return ("resp_prim", "payload")

        pgw = FakePublicGatewaySvc()
        fake_sm = types.SimpleNamespace(
            services={"publicGatewaySvc": pgw},
            ScatterEventWithoutTheStars=lambda *a, **kw: None,
            ScatterEvent=lambda *a, **kw: None,
            RemoteSvc=lambda *a, **kw: None,
            ProxySvc=lambda *a, **kw: None,
        )

        with mock.patch.object(hook, "_HOOKED_REMOTESVC", False), \
             mock.patch.object(hook, "_HOOK_RELOAD_GEN", 1), \
             mock.patch.object(hook, "_disable_background_focus_deferral"), \
             mock.patch.object(hook, "_wrap_message_owner"):
            hook._install_flight_recorder_hooks(fake_sm)

        # Reloading bumps generation and cleanly re-wraps without nesting the previous wrapper.
        first_wrapped = pgw.send_character_request
        self.assertTrue(getattr(first_wrapped, "_evomu_pt_wrapped", False))
        with mock.patch.object(hook, "_HOOKED_REMOTESVC", False), \
             mock.patch.object(hook, "_HOOK_RELOAD_GEN", 2), \
             mock.patch.object(hook, "_disable_background_focus_deferral"), \
             mock.patch.object(hook, "_wrap_message_owner"):
            hook._install_flight_recorder_hooks(fake_sm)
        self.assertIsNot(pgw.send_character_request, first_wrapped)
        self.assertIsNot(pgw.send_character_request._evomu_pt_orig, first_wrapped)

        # 1) Async send_character_request queues immediately -> NOT logged OK yet, and marks Quasar in-flight!
        _req_prim, rc = pgw.send_character_request(FakeDeleteRequest(), object, 15)
        self.assertEqual(hook._FLIGHT_RECORDER, [])
        self.assertTrue(hook._has_in_flight_quasar(fake_sm))

        # 2) _settle_after_action waits past max_ms=250 (simulating 350ms WAL commit) until rc.receive() finishes
        # AND waits one more 25ms tick after _FLIGHT_SEQ advances.
        settle_sleeps = []

        def fake_quasar_sleep(ms):
            settle_sleeps.append(ms)
            if sum(settle_sleeps) == 350:
                rc.receive()

        with mock.patch.object(hook, "_get_uicore", return_value=None), \
             mock.patch.object(hook.H, "_sleep_ms", create=True, side_effect=fake_quasar_sleep):
            waited = hook._settle_after_action(fake_sm, 0, 0, min_settle_ms=0, max_ms=250)

        self.assertEqual(waited, 375)
        self.assertEqual(len(hook._FLIGHT_RECORDER), 1)
        self.assertEqual(hook._FLIGHT_RECORDER[0]["kind"], "quasar")
        self.assertEqual(hook._FLIGHT_RECORDER[0]["args"], "send_character_request")
        self.assertEqual(hook._FLIGHT_RECORDER[0]["status"], "OK")
        self.assertFalse(hook._has_in_flight_quasar(fake_sm))

        # 3) Server non-200 failure during receive() records ERR with HTTP/gRPC status_code and clears in-flight.
        _rp2, err_rc = pgw.send_user_request(FakeDeleteRequest(), object, 15)
        self.assertTrue(hook._has_in_flight_quasar(fake_sm))
        with self.assertRaises(RuntimeError):
            err_rc.receive()
        self.assertFalse(hook._has_in_flight_quasar(fake_sm))
        self.assertEqual(hook._FLIGHT_RECORDER[-1]["status"], "ERR")
        self.assertIn("status=404", hook._FLIGHT_RECORDER[-1]["error"])

        # 4) Dropped / GC'd or expired channel never leaves _QUASAR_IN_FLIGHT stuck.
        dropped = pgw.send_request(FakeDeleteRequest())[1]
        self.assertTrue(hook._has_in_flight_quasar(fake_sm))
        del dropped
        gc.collect()
        self.assertFalse(hook._has_in_flight_quasar(fake_sm))

        blocking_res = pgw.send_blocking_character_request_and_receive_response(FakeDeleteRequest())
        self.assertEqual(blocking_res, ("resp_prim", "payload"))
        self.assertEqual(hook._FLIGHT_RECORDER[-1]["status"], "OK")
        self.assertFalse(hook._has_in_flight_quasar(fake_sm))

        # 5) native_broker pending_requests/pending_responses also prevents premature exit (while ignoring MagicMock).
        pgw.publicGateway = types.SimpleNamespace(
            get_request_broker_details=lambda: {"pending_requests": 1, "pending_responses": 0}
        )
        self.assertTrue(hook._has_in_flight_quasar(fake_sm))
        pgw.publicGateway = types.SimpleNamespace(
            get_request_broker_details=lambda: {"pending_requests": 0, "pending_responses": 0}
        )
        self.assertFalse(hook._has_in_flight_quasar(fake_sm))
        self.assertFalse(hook._has_in_flight_quasar(mock.MagicMock()))

        # Ensure _QUASAR_IN_FLIGHT globals exist and are listed in reload_hook _preserve_keys.
        hook_src = (HARNESS_DIR / "evomu_playtester_hook.py").read_text(encoding="utf-8")
        self.assertIn('"_QUASAR_IN_FLIGHT"', hook_src)
        self.assertIn('"_QUASAR_IN_FLIGHT_SEQ"', hook_src)

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

    def test_activate_module_resolves_display_names_and_rejects_unfitted(self):
        hook = _load_hook_with_py2_stubs()
        clicked = []

        class FakeModuleButton:
            def __init__(self, name, item_id, type_id, online=True, hint=""):
                self.name = name
                self.moduleinfo = types.SimpleNamespace(itemID=item_id, typeID=type_id)
                self.online = online
                self.hint = hint

            def OnClick(self):
                clicked.append(self.name)

        class FakeSlot:
            def __init__(self, name, children):
                self.name = name
                self.children = children

        btn_blaster = FakeModuleButton("ModuleButton_562", 5001, 562, online=True)
        btn_mwd = FakeModuleButton("ModuleButton_434", 5002, 434, online=True)
        btn_stale = FakeModuleButton("ModuleButton_483", 9999, 483, online=True)
        btn_hint_ii = FakeModuleButton("ModuleButton_440", 5004, 0, online=True, hint="<b>1MN Afterburner II</b><br>12.0 GJ")
        btn_hint = FakeModuleButton("ModuleButton_439", 5003, 0, online=True, hint="1MN Afterburner I\n10.0 GJ")

        # Direct unit checks on _module_button_display_name newline, <br>, blank-line, and label.text handling
        self.assertEqual(hook._module_button_display_name(btn_hint), "1MN Afterburner I")
        self.assertEqual(hook._module_button_display_name(btn_hint_ii), "1MN Afterburner II")
        btn_br_leading = FakeModuleButton(
            "ModuleButton_441",
            5005,
            0,
            online=True,
            hint="\n  \n<br/>ModuleButton_441\n  5MN   Microwarpdrive II  \nActivation: 10 GJ",
        )
        self.assertEqual(hook._module_button_display_name(btn_br_leading), "5MN Microwarpdrive II")
        btn_lbl_widget = types.SimpleNamespace(
            name="ModuleButton_518",
            moduleinfo=None,
            hint=None,
            label=types.SimpleNamespace(text="Small Shield Booster I<BR>5.0 GJ"),
        )
        self.assertEqual(hook._module_button_display_name(btn_lbl_widget), "Small Shield Booster I")

        slot_map = {
            "inFlightHighSlot1": FakeSlot("inFlightHighSlot1", [btn_blaster]),
            "inFlightHighSlot2": FakeSlot("inFlightHighSlot2", [btn_stale]),
            "inFlightMediumSlot1": FakeSlot("inFlightMediumSlot1", [btn_mwd]),
            "inFlightMediumSlot2": FakeSlot("inFlightMediumSlot2", [btn_hint_ii]),
            "inFlightMediumSlot3": FakeSlot("inFlightMediumSlot3", [btn_hint]),
            # Single-word lookup must NOT be shadowed by an offline widget returned from _find_widget
            "Afterburner": FakeModuleButton("ModuleButton_999", 5003, 439, online=False),
        }

        evetypes_mod = types.ModuleType("evetypes")
        evetypes_mod.GetName = lambda tid: {
            562: "Light Electron Blaster I",
            434: "5MN Microwarpdrive I",
            483: "Miner I",
            484: "Miner II",
        }.get(tid)

        park_states = types.SimpleNamespace(selected=1, activeTarget=2)
        parklife_pkg = types.ModuleType("eve.client.script.parklife")
        parklife_pkg.states = park_states

        st_svc = mock.MagicMock()
        inv_items = {
            9001: types.SimpleNamespace(typeID=649),
            5001: types.SimpleNamespace(typeID=562),
            5002: types.SimpleNamespace(typeID=434),
            5003: types.SimpleNamespace(typeID=439),
            5004: types.SimpleNamespace(typeID=440),
            5006: types.SimpleNamespace(typeID=483),
            5007: types.SimpleNamespace(typeID=484),
        }
        godma_lm = mock.MagicMock()
        godma_svc = types.SimpleNamespace(
            stateManager=types.SimpleNamespace(invItems=inv_items),
            GetDogmaLM=lambda: godma_lm,
        )
        sm_obj = mock.MagicMock()
        sm_obj.GetService.side_effect = lambda name: {"godma": godma_svc, "stateSvc": st_svc}.get(name)
        fake_sess = types.SimpleNamespace(shipid=9001)
        fake_uicore = types.SimpleNamespace(cmd=None, registry=None, layer=object(), desktop=None)

        def run_activate(target, extra_mods=None):
            mods = {
                "uthread": None,
                "evetypes": evetypes_mod,
                "eve": types.ModuleType("eve"),
                "eve.client": types.ModuleType("eve.client"),
                "eve.client.script": types.ModuleType("eve.client.script"),
                "eve.client.script.parklife": parklife_pkg,
                "eve.client.script.parklife.states": park_states,
            }
            if extra_mods:
                mods.update(extra_mods)
            with mock.patch.dict(sys.modules, mods), \
                 mock.patch.object(hook.__builtin__, "session", fake_sess, create=True), \
                 mock.patch.object(hook.__builtin__, "sm", sm_obj, create=True), \
                 mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                 mock.patch.object(hook, "_install_warp_telemetry"), \
                 mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
                 mock.patch.object(hook.H, "_sleep_ms", create=True), \
                 mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
                 mock.patch.object(hook, "_find_widget", side_effect=lambda tgt, layers=None: slot_map.get(tgt)), \
                 mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
                 mock.patch.object(hook, "_close_utilmenu_if_outside"), \
                 mock.patch.object(hook, "_close_context_menus_if_outside"), \
                 mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside", return_value=False):
                return hook._execute_action(sm_obj, {"type": "activate_module", "target": target, "wait_ms": 0})

        res_mwd = run_activate("5MN Microwarpdrive I")
        self.assertTrue(res_mwd.get("ok"), res_mwd)
        self.assertEqual(res_mwd.get("activated_slot"), "ModuleButton_434")
        self.assertEqual(clicked, ["ModuleButton_434"])

        st_svc.SetState.reset_mock()
        res_tgt = run_activate("5MN Microwarpdrive I: 750000301")
        self.assertTrue(res_tgt.get("ok"), res_tgt)
        self.assertEqual(res_tgt.get("activated_slot"), "ModuleButton_434")
        st_svc.SetState.assert_any_call(750000301, 1, 1)
        st_svc.SetState.assert_any_call(750000301, 2, 1)

        # Tier-0 exact multiline hint disambiguation: "1MN Afterburner I" must win over earlier "1MN Afterburner II"
        clicked_pre_ab = list(clicked)
        res_hint = run_activate("1MN Afterburner I", extra_mods={"evetypes": None})
        self.assertTrue(res_hint.get("ok"), res_hint)
        self.assertEqual(res_hint.get("activated_slot"), "ModuleButton_439")
        self.assertEqual(clicked, clicked_pre_ab + ["ModuleButton_439"])

        # Single-word display name must bypass _find_widget shadowing and choose online HUD button
        btn_hint_ii.online = False
        res_single = run_activate("Afterburner", extra_mods={"evetypes": None})
        self.assertTrue(res_single.get("ok"), res_single)
        self.assertEqual(res_single.get("activated_slot"), "ModuleButton_439")
        btn_hint_ii.online = True

        # Stale Miner I (itemID=9999 absent from godma invItems) must be skipped and fail closed.
        clicked_before = list(clicked)
        res_missing = run_activate("Miner I")
        self.assertFalse(res_missing.get("ok"), res_missing)
        self.assertEqual(res_missing.get("activated_slot"), "none_fitted")
        self.assertIn("not found on ship HUD rack", res_missing.get("error", ""))
        self.assertEqual(clicked, clicked_before)

        # Tier-0 offline exact match must NOT leak into Tier-1 online prefix match on another module (REV-02).
        btn_miner_offline = FakeModuleButton("ModuleButton_483_real", 5006, 483, online=False)
        btn_miner_ii_online = FakeModuleButton("ModuleButton_484", 5007, 484, online=True)
        slot_map["inFlightHighSlot2"] = FakeSlot("inFlightHighSlot2", [btn_miner_offline])
        slot_map["inFlightHighSlot3"] = FakeSlot("inFlightHighSlot3", [btn_miner_ii_online])
        res_tier_stop = run_activate("Miner I")
        self.assertTrue(res_tier_stop.get("ok"), res_tier_stop)
        self.assertEqual(res_tier_stop.get("activated_slot"), "ModuleButton_483_real")
        self.assertTrue(btn_miner_offline.online)
        godma_lm.SetModuleOnline.assert_called_once_with(9001, 5006)

        # /-qualified scene-graph HUD widget paths and lowercase slot keys must still resolve (REV-04).
        res_full_path = run_activate("inflight/l_shipui/slotsContainer/inFlightMediumSlot1/ModuleButton_434")
        self.assertTrue(res_full_path.get("ok"), res_full_path)
        self.assertEqual(res_full_path.get("activated_slot"), "ModuleButton_434")

        res_slot_path = run_activate("l_shipui/slotsContainer/inFlightHighSlot1")
        self.assertTrue(res_slot_path.get("ok"), res_slot_path)
        self.assertEqual(res_slot_path.get("activated_slot"), "ModuleButton_562")

        res_lower_hud = run_activate("inflighthighslot1")
        self.assertTrue(res_lower_hud.get("ok"), res_lower_hud)
        self.assertEqual(res_lower_hud.get("activated_slot"), "ModuleButton_562")

        res_lower_short = run_activate("medslot0")
        self.assertTrue(res_lower_short.get("ok"), res_lower_short)
        self.assertEqual(res_lower_short.get("activated_slot"), "ModuleButton_434")

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

    def test_input_text_alias_executes_type_text_on_quick_filter(self):
        hook = _load_hook_with_py2_stubs()
        set_values = []

        class FakeFilterEdit:
            name = "quickFilterInputBox"
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
             mock.patch.object(hook, "_find_widget", return_value=FakeFilterEdit()), \
             mock.patch.object(hook, "_resolve_editable_widget", side_effect=lambda n: n), \
             mock.patch.object(hook, "_settle_after_action", return_value=50):
            res = hook._execute_action(fake_sm, {"type": "input_text", "target": "InvContQuickFilter/quickFilterInputBox", "command": "1MN Afterburner I", "wait_ms": 300})
        self.assertTrue(res.get("ok"), res)
        self.assertEqual(res.get("typed_into"), "quickFilterInputBox")
        self.assertEqual(set_values, ["1MN Afterburner I"])

    def test_hotkey_alias_executes_press_key_command(self):
        hook = _load_hook_with_py2_stubs()
        called = []

        class FakeCmd:
            combatFunctionLoaded = None
            def OpenJournal(self):
                called.append("OpenJournal")

        fake_uicore = types.SimpleNamespace(cmd=FakeCmd(), registry=None, layer=None, desktop=None)
        fake_sm = mock.MagicMock()
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_settle_after_action", return_value=50):
            res = hook._execute_action(fake_sm, {"type": "hotkey", "target": None, "command": "alt+m", "wait_ms": 500})
        self.assertTrue(res.get("ok"), res)
        self.assertEqual(res.get("executed_cmd"), "OpenJournal")
        self.assertEqual(called, ["OpenJournal"])

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

    def test_click_xmpp_chat_channels_waits_for_async_tcp_join_settle(self):
        hook = _load_hook_with_py2_stubs()
        captured_settle = []

        xmpp_wnd = types.SimpleNamespace(name="XmppChatChannels", parent=None)
        row_entry = types.SimpleNamespace(name="entry_1", parent=xmpp_wnd)

        class FakeJoinBtn:
            name = "Button"
            parent = row_entry
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
             mock.patch.object(hook, "_find_widget", return_value=FakeJoinBtn()), \
             mock.patch.object(hook, "_find_widget_at_xy", return_value=FakeJoinBtn()), \
             mock.patch.object(hook, "_settle_after_action", side_effect=lambda *a, **kw: captured_settle.append(kw) or kw.get("min_settle_ms", 0)):
            res_widget = hook._execute_action(
                fake_sm,
                {
                    "type": "click_widget",
                    "target": "main/l_main/XmppChatChannels/content/main/scroll/maincontainer/__clipper/__content/entry_1/Button",
                    "wait_ms": 500,
                },
            )
            res_xy = hook._execute_action(
                fake_sm,
                {"type": "click_xy", "x": 1085, "y": 388, "wait_ms": 0},
            )
        self.assertTrue(res_widget.get("ok"), res_widget)
        self.assertTrue(res_xy.get("ok"), res_xy)
        self.assertEqual(len(captured_settle), 2)
        self.assertGreaterEqual(captured_settle[0]["min_settle_ms"], 350)
        self.assertGreaterEqual(captured_settle[1]["min_settle_ms"], 350)

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
        gm_ipc_calls = []

        class FakeTimeControl:
            def ClearSessionTimer(self):
                tc_calls.append("ClearSessionTimer")
            def AdvanceClock(self, secs):
                tc_calls.append(("AdvanceClock", secs))

        class FakeHostGmIpc:
            def request(self, payload):
                gm_ipc_calls.append(dict(payload))
                return {"ok": True, "via": "gm_bridge", "target_char_id": payload.get("target_char_id", 0)}

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
        fake_ipc = FakeHostGmIpc()
        with mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm}), \
             mock.patch.object(helpers, "_wait_view_settled"), \
             mock.patch.object(helpers, "_ensure_view"), \
             mock.patch.object(helpers, "_ensure_warp_signal_tracker"), \
             mock.patch.object(helpers, "_sleep_ms"), \
             mock.patch.object(helpers, "_sleep_sim_ms"):
            helpers._undock_ship(fake_sm, ipc=fake_ipc, allow_time_control=False)
            helpers._wait_warp_fx_settled(fake_sm, timeout_sec=0.05, allow_time_control=False)
            helpers._dock_station(fake_sm, 60003760, ipc=fake_ipc, allow_time_control=False)
            self.assertEqual(tc_calls, [])
            self.assertEqual(gm_ipc_calls, [])

            helpers._undock_ship(fake_sm, ipc=fake_ipc, allow_time_control=True)
            # Played client never invokes sm.RemoteSvc("timeControl"); routes via host GM IPC instead.
            self.assertEqual(tc_calls, [])
            self.assertTrue(any(c.get("cmd") == "clear_session_timer" and c.get("target_char_id") == 90000001 for c in gm_ipc_calls))

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

    def test_close_corporation_and_open_do_not_match_or_close_chat_window_stack(self):
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

            w.CloseByUser = _close_win
            w.Close = _close_win
            return w

        chat_stack = _make_win("ChatWindowStack", "ChatWindowStack", "Corp")
        chat_corp = _make_win("chatchannel_corp", "chatchannel_corp", "Corp")
        windows = [chat_stack, chat_corp]

        fake_reg = types.SimpleNamespace(
            GetWindows=lambda: list(windows),
            GetModalWindow=lambda: None,
            modals=[],
        )
        fake_uicore = types.SimpleNamespace(
            cmd=types.SimpleNamespace(OpenCorporationPanel=lambda: None),
            registry=fake_reg,
            layer=types.SimpleNamespace(modal=None, abovemain=None, main=None),
            desktop=types.SimpleNamespace(width=1280, height=800),
        )
        fake_viewstate = types.SimpleNamespace(
            secondaryInfo=None,
            activeView=None,
            GetCurrentViewInfo=lambda: types.SimpleNamespace(name="hangar"),
        )
        fake_sm = types.SimpleNamespace(services={"viewState": fake_viewstate}, GetService=lambda n: {"viewState": fake_viewstate}.get(n))
        sess = types.SimpleNamespace(
            userid=1, charid=90000001, stationid=60003760, structureid=None,
            shipid=100000001, solarsystemid=None, solarsystemid2=30000142, mutating=0, changing=None,
        )

        with mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_is_visible", side_effect=lambda obj: bool(getattr(obj, "display", True) and not getattr(obj, "destroyed", False))), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"session": {"charid": 90000001}, "open_windows": []}), \
             mock.patch.object(hook, "_settle_after_action", return_value=0), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook.H, "_sleep_ms", create=True):
            res_open_corp = hook._open_client_window(fake_sm, "corporation", require_open=True)
            self.assertFalse(res_open_corp.get("ok"), res_open_corp)
            self.assertIn("produced no visible window", str(res_open_corp.get("error", "")))
            self.assertIn("cmd=OpenCorporationPanel", str(res_open_corp.get("error", "")))

            res_tabs_corp = hook._click_top_tabs_in_window("corporation", require_match=True)
            self.assertFalse(res_tabs_corp.get("ok"), res_tabs_corp)
            self.assertIn("no visible open window found for click_top_tabs", str(res_tabs_corp.get("error", "")))

            corp_win = _make_win("corporation", "corporation", "Corporation")
            windows.append(corp_win)
            res_close_corp = hook._execute_action(fake_sm, {"type": "close_window", "target": "corporation", "wait_ms": 0})
            self.assertTrue(res_close_corp.get("action_ok"), res_close_corp)
            self.assertEqual(closed_wins, ["corporation"])
            self.assertTrue(corp_win.destroyed)
            self.assertFalse(chat_stack.destroyed)
            self.assertFalse(chat_corp.destroyed)

            closed_wins[:] = []
            res_close_stack = hook._execute_action(fake_sm, {"type": "close_window", "target": "ChatWindowStack", "wait_ms": 0})
            self.assertTrue(res_close_stack.get("action_ok"), res_close_stack)
            self.assertEqual(closed_wins, ["ChatWindowStack"])
            self.assertTrue(chat_stack.destroyed)


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


class TestDirectionalScannerOpenAndVisibility(unittest.TestCase):
    @staticmethod
    def _build_node(cls_name, name, x=0, y=0, w=100, h=30, parent=None, label="", click=False, editable=False, clip_children=False):
        def _init_attrs(self):
            self.name = name
            self._rect = (x, y, w, h)
            self.parent = parent
            self.children = []
            self.display = True
            self.destroyed = False
            self.state = 0
            self.label = label
            self.text = label
            self.clipChildren = clip_children
            self._click = click
            if editable:
                self.SetValue = lambda _v: None
            if parent is not None:
                parent.children.append(self)

        def _get_abs(self):
            return self._rect

        def _has_evt(self, evt):
            return bool(self._click and evt in ("OnClick", "OnDblClick"))

        cls = type(cls_name, (), {
            "__init__": _init_attrs,
            "GetAbsolute": _get_abs,
            "HasEventHandler": _has_evt,
        })
        return cls()

    def test_open_neocom_dscan_embedded_in_solar_system_map_exposes_window_and_scan_button_with_busy_overview(self):
        hook = _load_hook_with_py2_stubs()
        Node = self._build_node

        main_layer = Node("Layer", "l_main", 0, 0, 1708, 1036)
        side_layer = Node("Layer", "l_sidePanels", 0, 0, 1708, 1036)
        inflight_layer = Node("Layer", "l_inflight", 0, 0, 1708, 1036)

        ship_ui = Node("Container", "l_shipui", 700, 900, 300, 120, parent=inflight_layer)
        for idx in range(10):
            Node("ModuleButton", f"inFlightHighSlot{idx}", 710 + idx * 24, 910, 22, 22, parent=ship_ui, label=f"Gun {idx}", click=True)

        overview_wnd = Node("OverviewWindow", "overview", 1320, 40, 360, 700, parent=main_layer)
        overview_wnd.windowID = "overview"
        overview_wnd.caption = "Overview"
        ov_scroll = Node("ScrollContainer", "overviewscroll", 1325, 80, 350, 640, parent=overview_wnd)
        ov_clip = Node("Container", "clipCont", 1325, 80, 350, 640, parent=ov_scroll, clip_children=True)
        for idx in range(120):
            Node("OverviewScrollEntry", f"entry_{idx}", 1326, 82 + (idx % 20) * 20, 340, 18, parent=ov_clip, label=f"Gate {idx}", click=True)

        ss_panel = Node("SolarSystemViewPanel", "solar_system_map_panel", 48, 0, 1660, 1036, parent=side_layer)
        ss_panel.windowID = "solar_system_map_panel"
        ss_panel.caption = "Solar System Map"
        ss_panel.minimized = False
        ss_panel.IsMinimized = lambda: bool(ss_panel.minimized)
        ss_panel.Maximize = lambda: setattr(ss_panel, "minimized", False)

        # Realistic V24 nesting depth (ScanButton reaches depth 15)
        cur_parent = ss_panel
        for cname, nname in (
            ("Container", "content"),
            ("Container", "main"),
            ("MapViewSolarSystem", "MapViewSolarSystem"),
            ("Container", "mainCont"),
            ("Container", "HorizontalDragContainer"),
            ("Container", "mainCont"),
            ("Container", "PanelsContainer"),
            ("Container", "mainCont"),
            ("Container", "firstPanelMainCont"),
        ):
            cur_parent = Node(cname, nname, 48, 0, 460, 900, parent=cur_parent)
            if nname == "MapViewSolarSystem":
                ss_panel.mapView = cur_parent

        first_panel = cur_parent
        ss_panel.mapView.directionalScannerPalette = None
        state = {"dscan_open": False, "cmd_calls": []}
        windows = [overview_wnd, ss_panel]

        def _attach_embedded_palette():
            if state["dscan_open"] and ss_panel.mapView.directionalScannerPalette is not None:
                return
            pal = Node("DirectionalScannerPalette", "DirectionalScannerPalette", 60, 60, 420, 760, parent=first_panel, label="Scan Results", click=True)
            res_scroll = Node("BasicDynamicScroll", "scroll", 65, 100, 400, 560, parent=pal)
            res_clip = Node("Container", "clipCont", 65, 100, 400, 560, parent=res_scroll, clip_children=True)
            for idx in range(25):
                Node("ScrollEntry", f"dscan_row_{idx}", 66, 104 + (idx % 10) * 18, 390, 18, parent=res_clip, label=f"Ship {idx}", click=True)
            bot = Node("Container", "bottomCont", 65, 680, 400, 120, parent=pal)
            scan_cont = Node("Container", "scanButtonCont", 65, 685, 120, 40, parent=bot)
            Node("ScanButton", "ScanButton", 70, 690, 90, 30, parent=scan_cont, label="Scan", click=True)
            dist_cont = Node("Container", "DistanceEditCont", 200, 690, 140, 30, parent=bot)
            Node("SingleLineEditFloat", "auInput", 205, 692, 80, 24, parent=dist_cont, label="14.3", click=True, editable=True)
            ss_panel.mapView.directionalScannerPalette = pal
            state["dscan_open"] = True

        def _detach_embedded_palette():
            pal = ss_panel.mapView.directionalScannerPalette
            if pal is not None:
                pal.destroyed = True
                pal.display = False
                if pal in first_panel.children:
                    first_panel.children.remove(pal)
            ss_panel.mapView.directionalScannerPalette = None
            state["dscan_open"] = False

        ss_panel.mapView.CloseDirectionalScannerPalette = _detach_embedded_palette

        class FakeCmd:
            def OpenDirectionalScanner(self, toggle=True, *args):
                state["cmd_calls"].append(toggle)
                if toggle:
                    if state["dscan_open"]:
                        _detach_embedded_palette()
                    else:
                        _attach_embedded_palette()
                else:
                    _attach_embedded_palette()

        fake_layers = types.SimpleNamespace(main=main_layer, sidePanels=side_layer, inflight=inflight_layer)
        fake_reg = types.SimpleNamespace(GetWindows=lambda: list(windows), GetModalWindow=lambda: None)
        fake_uicore = types.SimpleNamespace(
            cmd=FakeCmd(),
            registry=fake_reg,
            layer=fake_layers,
            desktop=types.SimpleNamespace(width=1708, height=1036),
        )
        fake_sm = types.SimpleNamespace(services={}, GetService=lambda _n: None)
        space_sess = types.SimpleNamespace(stationid=None, structureid=None, solarsystemid=30000144, solarsystemid2=30000144, shipid=1001)

        def _snap(*_a, **_kw):
            tree = hook._inspect_uicore_tree()
            return {
                "open_windows": tree.get("open_windows") or [],
                "modals": tree.get("modals") or [],
                "ui": tree,
                "space": {},
                "skills_state": {},
                "wallet_state": {},
                "active_view": "inflight",
            }

        with mock.patch.dict(builtins.__dict__, {"session": space_sess, "sm": fake_sm, "uicore": fake_uicore}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook, "_get_state_snapshot", side_effect=_snap):
            pre_tree = hook._inspect_uicore_tree()
            self.assertFalse(any(w.get("window_id") == "directionalScannerWindow" for w in pre_tree["open_windows"]))

            # 1) Opening dscan without require_window_open=True opens embedded D-Scan and exposes it + ScanButton
            res = hook._execute_action(fake_sm, {"type": "open_neocom", "target": "dscan", "wait_ms": 0})
            self.assertTrue(res.get("ok"), res)
            self.assertTrue(res.get("action_ok"), res)
            self.assertIn("directionalScannerWindow", (res.get("state_diff") or {}).get("windows_opened", []), res)
            self.assertTrue(state["dscan_open"])

            post_tree = hook._inspect_uicore_tree()
            ds_wins = [w for w in post_tree["open_windows"] if w.get("window_id") == "directionalScannerWindow"]
            self.assertEqual(len(ds_wins), 1, post_tree["open_windows"])
            self.assertTrue(ds_wins[0].get("embedded"))
            self.assertEqual(ds_wins[0].get("host_window_id"), "solar_system_map_panel")

            cw = post_tree["clickable_widgets"]
            self.assertLessEqual(len(cw), 140)
            self.assertTrue(any(w.get("cls") == "ScanButton" and w.get("label") == "Scan" for w in cw), cw)
            self.assertTrue(any(w.get("name") == "auInput" for w in cw), cw)
            scan_node = hook._find_widget("Scan", fake_layers)
            self.assertIsNotNone(scan_node)
            self.assertEqual(scan_node.__class__.__name__, "ScanButton")

            tab_res = hook._click_top_tabs_in_window("dscan", max_tabs=4, require_match=True)
            self.assertTrue(tab_res.get("ok"), tab_res)
            self.assertEqual(tab_res.get("surface_type"), "embedded_panel")

            # 2) Calling open_neocom('dscan') while already open NEVER toggles embedded D-Scan closed
            calls_before_second = len(state["cmd_calls"])
            res2 = hook._execute_action(fake_sm, {"type": "open_neocom", "target": "dscan", "wait_ms": 0})
            self.assertTrue(res2.get("ok"), res2)
            self.assertTrue(res2.get("action_ok"), res2)
            self.assertTrue(state["dscan_open"])
            self.assertEqual(len(state["cmd_calls"]), calls_before_second)

            # 3) Explicit close_modal_or_window('dscan') closes only embedded D-Scan palette, keeping SolarSystemViewPanel
            res_close = hook._execute_action(fake_sm, {"type": "close_modal_or_window", "target": "dscan", "wait_ms": 0})
            self.assertTrue(res_close.get("action_ok"), res_close)
            self.assertFalse(state["dscan_open"])
            self.assertIn(ss_panel, windows)
            after_close_tree = hook._inspect_uicore_tree()
            self.assertFalse(any(w.get("window_id") == "directionalScannerWindow" for w in after_close_tree["open_windows"]))

    def test_open_neocom_dscan_standalone_and_minimized_maximize(self):
        hook = _load_hook_with_py2_stubs()
        Node = self._build_node

        main_layer = Node("Layer", "l_main", 0, 0, 1280, 800)
        windows = []
        cmd_calls = []

        def _make_standalone(minimized=False):
            wnd = Node("DirectionalScanner", "directionalScannerWindow", 120, 80, 420, 520, parent=main_layer)
            wnd.windowID = "directionalScannerWindow"
            wnd.caption = "Directional Scanner"

            def _minimize():
                wnd.minimized = True
                wnd.state = 2  # CarbonUI uiconst.UI_HIDDEN

            def _maximize():
                wnd.minimized = False
                wnd.state = 0  # CarbonUI uiconst.UI_NORMAL

            wnd.Minimize = _minimize
            wnd.Maximize = _maximize
            wnd.IsMinimized = lambda: bool(wnd.minimized)
            if minimized:
                _minimize()
            else:
                _maximize()
            pal = Node("DirectionalScannerPalette", "DirectionalScannerPalette", 125, 110, 410, 480, parent=wnd)
            Node("ScanButton", "ScanButton", 140, 540, 90, 28, parent=pal, label="Scan", click=True)
            return wnd

        class FakeCmd:
            def OpenDirectionalScanner(self):
                cmd_calls.append("OpenDirectionalScanner")
                if not windows:
                    windows.append(_make_standalone(minimized=True))

        fake_uicore = types.SimpleNamespace(
            cmd=FakeCmd(),
            registry=types.SimpleNamespace(GetWindows=lambda: list(windows), GetModalWindow=lambda: None),
            layer=types.SimpleNamespace(main=main_layer),
            desktop=types.SimpleNamespace(width=1280, height=800),
        )
        fake_sm = types.SimpleNamespace(services={}, GetService=lambda _n: None)
        space_sess = types.SimpleNamespace(stationid=None, structureid=None, solarsystemid=30000144, shipid=1001)

        with mock.patch.dict(builtins.__dict__, {"session": space_sess, "sm": fake_sm, "uicore": fake_uicore}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"):
            res = hook._open_client_window(fake_sm, "dscan")
            self.assertTrue(res.get("ok"), res)
            self.assertEqual(len(windows), 1)
            self.assertFalse(windows[0].minimized)
            self.assertEqual(windows[0].state, 0)
            ui_tree = hook._inspect_uicore_tree()
            self.assertTrue(any(w.get("window_id") == "directionalScannerWindow" for w in ui_tree["open_windows"]))
            self.assertTrue(any(w.get("cls") == "ScanButton" and w.get("label") == "Scan" for w in ui_tree["clickable_widgets"]))

            # Minimize existing standalone window with CarbonUI UI_HIDDEN (state=2) and reopen without calling command
            windows[0].Minimize()
            self.assertEqual(windows[0].state, 2)
            calls_before = len(cmd_calls)
            res2 = hook._open_client_window(fake_sm, "dscan")
            self.assertTrue(res2.get("ok"), res2)
            self.assertFalse(windows[0].minimized)
            self.assertEqual(windows[0].state, 0)
            self.assertEqual(len(cmd_calls), calls_before)

    def test_open_neocom_dscan_minimized_solar_system_map_restores_preserves_toggle_and_closes(self):
        hook = _load_hook_with_py2_stubs()
        Node = self._build_node

        side_layer = Node("Layer", "l_sidePanels", 0, 0, 1280, 800)
        ss_panel = Node("SolarSystemViewPanel", "solar_system_map_panel", 48, 0, 1232, 800, parent=side_layer)
        ss_panel.windowID = "solar_system_map_panel"
        ss_panel.caption = "Solar System Map"
        ss_panel.Minimize = lambda: (setattr(ss_panel, "minimized", True), setattr(ss_panel, "state", 2))
        ss_panel.Maximize = lambda: (setattr(ss_panel, "minimized", False), setattr(ss_panel, "state", 0))
        ss_panel.IsMinimized = lambda: bool(getattr(ss_panel, "minimized", False))
        ss_panel.Minimize()

        mv = Node("MapViewSolarSystem", "MapViewSolarSystem", 48, 0, 1232, 800, parent=ss_panel)
        ss_panel.mapView = mv
        mv.directionalScannerPalette = None
        state = {"dscan_open": False, "cmd_calls": []}

        def _attach_pal():
            if mv.directionalScannerPalette is not None and not mv.directionalScannerPalette.destroyed:
                return
            pal = Node("DirectionalScannerPalette", "DirectionalScannerPalette", 60, 60, 420, 620, parent=mv, click=True)
            pal.minimized = False
            pal.Minimize = lambda: (setattr(pal, "minimized", True), setattr(pal, "state", 2))
            pal.Maximize = lambda: (setattr(pal, "minimized", False), setattr(pal, "state", 0))
            pal.IsMinimized = lambda: bool(getattr(pal, "minimized", False))
            Node("ScanButton", "ScanButton", 70, 630, 90, 28, parent=pal, label="Scan", click=True)
            mv.directionalScannerPalette = pal
            state["dscan_open"] = True

        def _detach_pal():
            pal = mv.directionalScannerPalette
            if pal is not None:
                pal.destroyed = True
                pal.display = False
                if pal in mv.children:
                    mv.children.remove(pal)
            mv.directionalScannerPalette = None
            state["dscan_open"] = False

        mv.CloseDirectionalScannerPalette = _detach_pal

        class FakeCmd:
            def OpenDirectionalScanner(self, toggle=True, *args):
                state["cmd_calls"].append(toggle)
                if toggle:
                    if state["dscan_open"]:
                        _detach_pal()
                    else:
                        _attach_pal()
                else:
                    _attach_pal()

        windows = [ss_panel]
        fake_layers = types.SimpleNamespace(sidePanels=side_layer)
        fake_uicore = types.SimpleNamespace(
            cmd=FakeCmd(),
            registry=types.SimpleNamespace(GetWindows=lambda: list(windows), GetModalWindow=lambda: None),
            layer=fake_layers,
            desktop=types.SimpleNamespace(width=1280, height=800),
        )
        fake_sm = types.SimpleNamespace(services={}, GetService=lambda _n: None)
        space_sess = types.SimpleNamespace(stationid=None, structureid=None, solarsystemid=30000144, shipid=1001)
        empty_snap = {"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}

        with mock.patch.dict(builtins.__dict__, {"session": space_sess, "sm": fake_sm, "uicore": fake_uicore}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value=empty_snap):
            # 1) Minimized SolarSystemViewPanel (state=2) with closed D-Scan passes toggle=False and maximizes host
            res1 = hook._execute_action(fake_sm, {"type": "open_neocom", "target": "dscan", "wait_ms": 0})
            self.assertTrue(res1.get("ok"), res1)
            self.assertTrue(res1.get("action_ok"), res1)
            self.assertEqual(state["cmd_calls"], [False])
            self.assertFalse(ss_panel.minimized)
            self.assertEqual(ss_panel.state, 0)
            self.assertTrue(state["dscan_open"])

            # 2) Minimized SolarSystemViewPanel (state=2) with open D-Scan restores host without toggling D-Scan closed
            ss_panel.Minimize()
            self.assertFalse(any(w.get("window_id") == "directionalScannerWindow" for w in hook._inspect_uicore_tree()["open_windows"]))
            res2 = hook._execute_action(fake_sm, {"type": "open_neocom", "target": "dscan", "wait_ms": 0})
            self.assertTrue(res2.get("ok"), res2)
            self.assertFalse(ss_panel.minimized)
            self.assertEqual(ss_panel.state, 0)
            self.assertEqual(state["cmd_calls"], [False])
            self.assertTrue(any(w.get("window_id") == "directionalScannerWindow" for w in hook._inspect_uicore_tree()["open_windows"]))

            # 3) Minimized DirectionalScannerPalette inside visible SolarSystemViewPanel is hidden until maximized
            mv.directionalScannerPalette.Minimize()
            self.assertFalse(any(w.get("window_id") == "directionalScannerWindow" for w in hook._inspect_uicore_tree()["open_windows"]))
            res3 = hook._execute_action(fake_sm, {"type": "open_neocom", "target": "dscan", "wait_ms": 0})
            self.assertTrue(res3.get("ok"), res3)
            self.assertFalse(mv.directionalScannerPalette.minimized)
            self.assertEqual(mv.directionalScannerPalette.state, 0)

            # 4) Closing dscan while SolarSystemViewPanel is minimized (state=2) still closes embedded D-Scan palette
            ss_panel.Minimize()
            res_close = hook._execute_action(fake_sm, {"type": "close_modal_or_window", "target": "dscan", "wait_ms": 0})
            self.assertTrue(res_close.get("action_ok"), res_close)
            self.assertFalse(state["dscan_open"])
            self.assertIn(ss_panel, windows)

    def test_open_neocom_dscan_fails_closed_when_missing_hidden_minimized_or_docked(self):
        hook = _load_hook_with_py2_stubs()
        Node = self._build_node

        main_layer = Node("Layer", "l_main", 0, 0, 1280, 800)
        side_layer = Node("Layer", "l_sidePanels", 0, 0, 1280, 800)
        ss_panel = Node("SolarSystemViewPanel", "solar_system_map_panel", 48, 0, 1232, 800, parent=side_layer)
        ss_panel.windowID = "solar_system_map_panel"
        ss_panel.mapView = types.SimpleNamespace(directionalScannerPalette=None)
        # Hidden/closed palette left on mapView must not satisfy open_windows or open check
        hidden_pal = Node("DirectionalScannerPalette", "DirectionalScannerPalette", 60, 60, 400, 600, parent=ss_panel)
        hidden_pal.display = False
        ss_panel.mapView.directionalScannerPalette = hidden_pal

        cmd_calls = []

        class NoopCmd:
            def OpenDirectionalScanner(self, toggle=True, *args):
                cmd_calls.append(toggle)

        windows = [ss_panel]
        fake_uicore = types.SimpleNamespace(
            cmd=NoopCmd(),
            registry=types.SimpleNamespace(GetWindows=lambda: list(windows), GetModalWindow=lambda: None),
            layer=types.SimpleNamespace(main=main_layer, sidePanels=side_layer),
            desktop=types.SimpleNamespace(width=1280, height=800),
        )
        fake_sm = types.SimpleNamespace(services={}, GetService=lambda _n: None)
        space_sess = types.SimpleNamespace(stationid=None, structureid=None, solarsystemid=30000144, shipid=1001)
        docked_sess = types.SimpleNamespace(stationid=60003760, structureid=None, solarsystemid=None, shipid=1001)
        empty_snap = {"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}

        with mock.patch.dict(builtins.__dict__, {"session": space_sess, "sm": fake_sm, "uicore": fake_uicore}), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value=empty_snap):
            tree = hook._inspect_uicore_tree()
            self.assertFalse(any(w.get("window_id") == "directionalScannerWindow" for w in tree["open_windows"]))

            # Without require_window_open=True, no-op OpenDirectionalScanner still fails closed
            res_default = hook._execute_action(fake_sm, {"type": "open_neocom", "target": "dscan", "wait_ms": 0})
            self.assertFalse(res_default.get("ok"), res_default)
            self.assertFalse(res_default.get("action_ok"), res_default)
            self.assertIn("Directional Scanner", res_default.get("error", ""))
            self.assertEqual(len(cmd_calls), 1)

            # With require_window_open=True, also fails closed
            res_req = hook._execute_action(fake_sm, {"type": "open_window", "target": "dscan", "require_window_open": True, "wait_ms": 0})
            self.assertFalse(res_req.get("ok"), res_req)
            self.assertFalse(res_req.get("action_ok"), res_req)
            self.assertEqual(len(cmd_calls), 2)

            # Un-maximizable minimized SolarSystemViewPanel (state=2) with attached embedded palette fails closed
            ss_panel.minimized = True
            ss_panel.state = 2
            ss_panel.Maximize = lambda: None  # Maximize fails to restore
            hidden_pal.display = True
            hidden_pal.state = 0
            tree_min = hook._inspect_uicore_tree()
            self.assertFalse(any(w.get("window_id") == "directionalScannerWindow" for w in tree_min["open_windows"]))
            res_unmax_emb = hook._execute_action(fake_sm, {"type": "open_neocom", "target": "dscan", "wait_ms": 0})
            self.assertFalse(res_unmax_emb.get("ok"), res_unmax_emb)
            self.assertFalse(res_unmax_emb.get("action_ok"), res_unmax_emb)
            self.assertEqual(cmd_calls[-1], False)

            # Un-maximizable minimized standalone DirectionalScanner (state=2, no working Maximize) fails closed
            stuck_ds = Node("DirectionalScanner", "directionalScannerWindow", 100, 100, 400, 500, parent=main_layer)
            stuck_ds.windowID = "directionalScannerWindow"
            stuck_ds.minimized = True
            stuck_ds.state = 2
            stuck_ds.Maximize = lambda: None
            windows[:] = [stuck_ds]
            res_unmax_std = hook._execute_action(fake_sm, {"type": "open_neocom", "target": "dscan", "wait_ms": 0})
            self.assertFalse(res_unmax_std.get("ok"), res_unmax_std)
            self.assertFalse(res_unmax_std.get("action_ok"), res_unmax_std)

            # While docked, fails closed with space-only guard without calling cmd
            calls_before_dock = len(cmd_calls)
            with mock.patch.dict(builtins.__dict__, {"session": docked_sess}):
                res_docked = hook._execute_action(fake_sm, {"type": "open_neocom", "target": "dscan", "wait_ms": 0})
                self.assertFalse(res_docked.get("ok"), res_docked)
                self.assertFalse(res_docked.get("action_ok"), res_docked)
                self.assertIn("in space", res_docked.get("error", ""))
                self.assertEqual(len(cmd_calls), calls_before_dock)

    def test_dock_skips_warp_when_client_already_docked_during_warp_settle(self):
        """Regression test for #1421: background CmdDock completing during _wait_warp_fx_settled must not call CmdWarpToStuff."""
        hook = _load_hook_with_py2_stubs()
        warp_calls = []
        dock_calls = []
        fake_sess = types.SimpleNamespace(
            solarsystemid=30000144,
            solarsystemid2=30000144,
            stationid=None,
            structureid=None,
            shipid=1897928255,
            mutating=0,
            changing=None,
        )

        class FakeBallpark:
            slimItems = {60006880: types.SimpleNamespace(categoryID=3)}

            def GetBall(self, bid):
                if fake_sess.stationid is not None:
                    return None
                if bid == 1897928255:
                    return types.SimpleNamespace(mode=3)
                return None

        fake_bp = FakeBallpark()
        fake_michelle = types.SimpleNamespace(GetBallpark=lambda: fake_bp)
        fake_beyonce = types.SimpleNamespace(
            CmdWarpToStuff=lambda *args: warp_calls.append(args),
        )
        fake_sm = mock.Mock()
        fake_sm.IsServiceRunning.return_value = False
        fake_sm.services = {"michelle": fake_michelle}
        fake_sm.GetService.side_effect = lambda name: {
            "michelle": fake_michelle,
            "viewState": types.SimpleNamespace(secondaryInfo=None),
        }.get(name)
        fake_sm.RemoteSvc.side_effect = lambda name: {
            "timeControl": types.SimpleNamespace(ClearSessionTimer=lambda: None, AdvanceClock=lambda _s: None),
            "stationSvc": types.SimpleNamespace(Dock=lambda sid: dock_calls.append(sid)),
        }[name]
        fake_es = types.SimpleNamespace(_EXC_SEQ=0, _RECENT_EXCEPTIONS=[], _PT_CONSUMED_EXC_SEQ=0, _REPORTED_LOG_EXC_SIGS=set())

        def _finish_bg_dock(*args, **kwargs):
            # Simulate client background CmdDock completing right as warp ends
            fake_sess.stationid = 60006880
            fake_sess.solarsystemid = None

        with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
             mock.patch.object(builtins, "session", fake_sess, create=True), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook.H, "_get_beyonce", return_value=fake_beyonce, create=True), \
             mock.patch.object(hook.H, "_wait_warp_fx_settled", side_effect=_finish_bg_dock, create=True), \
             mock.patch.object(hook.H, "_wait_view_settled", create=True), \
             mock.patch.object(hook.H, "_ensure_view", create=True), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook.H, "_sleep_sim_ms", create=True):
            res = hook._execute_action(fake_sm, {"type": "dock", "target": "60006880"})

        self.assertTrue(res.get("ok"), res)
        self.assertEqual(warp_calls, [])
        self.assertEqual(dock_calls, [])


class TestFittingShipSwapStaleKeyErrorGuard(unittest.TestCase):
    """Regression tests for #1410: V24 fittingWnd stale-ship KeyError during ship swap."""

    def _load_fresh_evomu_site(self):
        sys.modules.setdefault("__builtin__", builtins)
        site_path = HARNESS_DIR / "evomu_site.py"
        spec = importlib.util.spec_from_file_location("evomu_site_1410_test", site_path)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod

    def setUp(self):
        self._saved_modules = dict(sys.modules)
        self._saved_session = getattr(builtins, "session", None)
        self._saved_sm = getattr(builtins, "sm", None)
        self.site = self._load_fresh_evomu_site()

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

    def _make_fake_dogma_and_controller(
        self,
        old_ship_id=1897898652,
        new_ship_id=1896955150,
        simulated=False,
    ):
        class FakeDogmaLocation:
            def __init__(self):
                self.dogmaItems = {old_ship_id: object()}
                self.current_ship_id = old_ship_id
                self.shipIDBeingDisembarked = None
                self.shipIDBeingEmbarked = None

            def GetCurrentShipID(self):
                return self.current_ship_id

            def IsSwitchingShips(self):
                return self.shipIDBeingDisembarked is not None

        dl = FakeDogmaLocation()

        class FakeFittingController:
            def __init__(self):
                self._itemID = old_ship_id
                self.dogmaLocation = dl
                self._simulated = simulated

            def GetItemID(self):
                return self._itemID

            def GetDogmaLocation(self):
                return self.dogmaLocation

            def IsSimulated(self):
                return self._simulated

        ctrl = FakeFittingController()
        return dl, ctrl

    def _register_fitting_modules(self, eval_fn, icon_fn, stats_fn, stats_drone_fn, drone_panel_fn):
        fw_mod = types.ModuleType("shipfitting.fittingWarnings")
        fw_mod.EvaluateFit = eval_fn
        sys.modules["shipfitting.fittingWarnings"] = fw_mod

        wic_mod = types.ModuleType("eve.client.script.ui.shared.fittingScreen.warningIconCont")

        class WarningIconCont:
            def __init__(self, controller, destroyed=False):
                self.fittingController = controller
                self.destroyed = destroyed

            def __UpdateIcon_thread(self):
                return icon_fn(self)

        wic_mod.WarningIconCont = WarningIconCont
        sys.modules["eve.client.script.ui.shared.fittingScreen.warningIconCont"] = wic_mod

        sp_mod = types.ModuleType("eve.client.script.ui.shared.fittingScreen.statsPanel")

        class StatsPanel:
            def __init__(self, controller, drone_panel=None, destroyed=False):
                self.controller = controller
                self.droneStatsParent = drone_panel
                self.destroyed = destroyed

            def UpdateStats(self):
                return stats_fn(self)

            def UpdateDroneStats(self):
                return stats_drone_fn(self)

        sp_mod.StatsPanel = StatsPanel
        sys.modules["eve.client.script.ui.shared.fittingScreen.statsPanel"] = sp_mod

        dp_mod = types.ModuleType("eve.client.script.ui.shared.fittingScreen.fittingPanels.dronePanel")

        class DronePanel:
            def __init__(self, controller, destroyed=False):
                self.controller = controller
                self.destroyed = destroyed

            def UpdateDroneStats(self):
                return drone_panel_fn(self)

        dp_mod.DronePanel = DronePanel
        sys.modules["eve.client.script.ui.shared.fittingScreen.fittingPanels.dronePanel"] = dp_mod

        self.assertTrue(self.site._install_fitting_active_ship_guard())
        return fw_mod, WarningIconCont, StatsPanel, DronePanel

    def test_1410_stale_ship_keyerror_during_ship_swap_suppressed_cleanly(self):
        old_ship = 1897898652
        new_ship = 1896955150
        dl, ctrl = self._make_fake_dogma_and_controller(old_ship, new_ship)
        builtins.session = types.SimpleNamespace(shipid=old_ship, stationid=60003760)

        def simulate_unload_and_raise_stale():
            # Simulate _MakeShipActive unloading old_ship during an RPC yield while
            # session.shipid has already transitioned to new_ship.
            dl.shipIDBeingDisembarked = old_ship
            dl.shipIDBeingEmbarked = new_ship
            builtins.session.shipid = new_ship
            dl.dogmaItems.pop(old_ship, None)
            raise KeyError(old_ship)

        def simulate_full_swap_and_raise_stale():
            # Even if ProcessActiveShipChanged already updated controller._itemID and
            # cleared shipIDBeingDisembarked before the yielded tasklet resumed,
            # initial_ship_ids captured at entry still identifies old_ship.
            builtins.session.shipid = new_ship
            dl.current_ship_id = new_ship
            dl.shipIDBeingDisembarked = None
            dl.shipIDBeingEmbarked = None
            dl.dogmaItems = {new_ship: object()}
            ctrl._itemID = new_ship
            raise KeyError(old_ship)

        mode = {"fn": simulate_unload_and_raise_stale}

        fw_mod, WarningIconCont, StatsPanel, DronePanel = self._register_fitting_modules(
            eval_fn=lambda _ctrl: mode["fn"](),
            icon_fn=lambda _self: mode["fn"](),
            stats_fn=lambda self: self.UpdateDroneStats(),
            stats_drone_fn=lambda self: self.droneStatsParent.UpdateDroneStats(),
            drone_panel_fn=lambda _self: mode["fn"](),
        )

        # Mid-swap yield: all 4 paths suppress stale old_ship KeyError cleanly.
        dp = DronePanel(ctrl)
        sp = StatsPanel(ctrl, drone_panel=dp)
        wic = WarningIconCont(ctrl)
        self.assertIsNone(fw_mod.EvaluateFit(ctrl))
        self.assertIsNone(wic._WarningIconCont__UpdateIcon_thread())
        self.assertIsNone(sp.UpdateStats())
        self.assertIsNone(sp.UpdateDroneStats())
        self.assertIsNone(dp.UpdateDroneStats())

        # Post-swap yield: controller and dogmaLocation already settled on new_ship
        # while the resumed tasklet still indexed old_ship.
        mode["fn"] = simulate_full_swap_and_raise_stale
        for runner in (
            lambda: fw_mod.EvaluateFit(ctrl),
            lambda: wic._WarningIconCont__UpdateIcon_thread(),
            lambda: sp.UpdateStats(),
            lambda: dp.UpdateDroneStats(),
        ):
            dl.dogmaItems = {old_ship: object()}
            dl.current_ship_id = old_ship
            ctrl._itemID = old_ship
            builtins.session.shipid = old_ship
            self.assertIsNone(runner())

        # Destroyed panels short-circuit cleanly without invoking body.
        called = []
        destroyed_sp = StatsPanel(ctrl, drone_panel=dp, destroyed=True)
        destroyed_dp = DronePanel(ctrl, destroyed=True)
        mode["fn"] = lambda: called.append("ran")
        self.assertIsNone(destroyed_sp.UpdateStats())
        self.assertIsNone(destroyed_dp.UpdateDroneStats())
        self.assertEqual(called, [])

    def test_1410_unrelated_keyerrors_and_non_keyerrors_still_propagate(self):
        old_ship = 1897898652
        new_ship = 1896955150
        dl, ctrl = self._make_fake_dogma_and_controller(old_ship, new_ship)
        builtins.session = types.SimpleNamespace(shipid=new_ship, stationid=60003760)
        dl.shipIDBeingDisembarked = old_ship
        dl.shipIDBeingEmbarked = new_ship
        dl.dogmaItems = {new_ship: object()}

        exc_box = {"exc": None}

        def raise_box(*_args, **_kwargs):
            raise exc_box["exc"]

        fw_mod, WarningIconCont, StatsPanel, DronePanel = self._register_fitting_modules(
            eval_fn=raise_box,
            icon_fn=raise_box,
            stats_fn=raise_box,
            stats_drone_fn=raise_box,
            drone_panel_fn=raise_box,
        )

        dp = DronePanel(ctrl)
        sp = StatsPanel(ctrl, drone_panel=dp)
        wic = WarningIconCont(ctrl)
        targets = (
            ("EvaluateFit", lambda: fw_mod.EvaluateFit(ctrl)),
            ("WarningIconCont", lambda: wic._WarningIconCont__UpdateIcon_thread()),
            ("StatsPanel.UpdateStats", lambda: sp.UpdateStats()),
            ("StatsPanel.UpdateDroneStats", lambda: sp.UpdateDroneStats()),
            ("DronePanel.UpdateDroneStats", lambda: dp.UpdateDroneStats()),
        )

        # 1) Missing dogma attribute IDs (e.g. 1271 droneBandwidth, 14 hiSlots) MUST propagate even during ship swap.
        for bad_exc in (
            KeyError(1271),
            KeyError(14),
            KeyError(1897899999),  # unrelated item ID
            KeyError("missing_attr"),
            KeyError(True),
            ValueError("unexpected value"),
            RuntimeError("unexpected runtime error"),
            IndexError("list index out of range"),
        ):
            exc_box["exc"] = bad_exc
            for label, fn in targets:
                with self.subTest(case=repr(bad_exc), target=label):
                    with self.assertRaises(type(bad_exc)):
                        fn()

        # 2) KeyError(old_ship) MUST propagate if old_ship is STILL in dogmaItems (error from another dict).
        dl.dogmaItems = {old_ship: object(), new_ship: object()}
        exc_box["exc"] = KeyError(old_ship)
        for label, fn in targets:
            with self.subTest(case="still_in_dogmaItems", target=label):
                with self.assertRaises(KeyError):
                    fn()

        # 3) KeyError(old_ship) MUST propagate when NOT switching ships and session/dogma/controller agree on old_ship.
        dl.dogmaItems = {}
        dl.shipIDBeingDisembarked = None
        dl.shipIDBeingEmbarked = None
        dl.current_ship_id = old_ship
        ctrl._itemID = old_ship
        builtins.session.shipid = old_ship
        exc_box["exc"] = KeyError(old_ship)
        for label, fn in targets:
            with self.subTest(case="settled_active_ship_missing", target=label):
                with self.assertRaises(KeyError):
                    fn()

        # 4) Simulated fitting controller KeyError MUST propagate even when session.shipid != simulated ship ID.
        sim_dl, sim_ctrl = self._make_fake_dogma_and_controller(old_ship, new_ship, simulated=True)
        sim_dl.dogmaItems = {}
        builtins.session.shipid = new_ship
        exc_box["exc"] = KeyError(old_ship)
        sim_dp = DronePanel(sim_ctrl)
        sim_sp = StatsPanel(sim_ctrl, drone_panel=sim_dp)
        sim_wic = WarningIconCont(sim_ctrl)
        for label, fn in (
            ("EvaluateFit", lambda: fw_mod.EvaluateFit(sim_ctrl)),
            ("WarningIconCont", lambda: sim_wic._WarningIconCont__UpdateIcon_thread()),
            ("StatsPanel.UpdateStats", lambda: sim_sp.UpdateStats()),
            ("DronePanel.UpdateDroneStats", lambda: sim_dp.UpdateDroneStats()),
        ):
            with self.subTest(case="simulated_controller", target=label):
                with self.assertRaises(KeyError):
                    fn()

    def test_1410_meta_hook_schedules_post_import_guard_for_fitting_panels(self):
        scheduled = []
        sys.modules["uthread"] = types.SimpleNamespace(new=lambda fn: scheduled.append(fn))
        hook = self.site._EvomuMetaHook()
        for mod_name in (
            "shipfitting.fittingWarnings",
            "eve.client.script.ui.shared.fittingScreen.warningIconCont",
            "eve.client.script.ui.shared.fittingScreen.statsPanel",
            "eve.client.script.ui.shared.fittingScreen.fittingPanels.dronePanel",
        ):
            scheduled.clear()
            self.assertIsNone(hook.find_module(mod_name))
            self.assertEqual(scheduled, [self.site._install_fitting_active_ship_guard])

    def test_1443_partially_initialized_fitting_modules_not_marked_until_callable_wrapped(self):
        old_ship = 1897898652
        new_ship = 1896955150
        dl, ctrl = self._make_fake_dogma_and_controller(old_ship, new_ship)
        builtins.session = types.SimpleNamespace(shipid=old_ship, stationid=60003760)

        scheduled = []
        sys.modules["uthread"] = types.SimpleNamespace(new=lambda fn: scheduled.append(fn))
        hook = self.site._EvomuMetaHook()

        # 1) Top-level import of fittingWarnings schedules the deferred uthread callback.
        self.assertIsNone(hook.find_module("shipfitting.fittingWarnings"))
        self.assertEqual(scheduled, [self.site._install_fitting_active_ship_guard])

        # 2) Simulate partially-initialized modules in sys.modules BEFORE their callables
        #    are bound (e.g. during nested top-level imports inside each module).
        fw_mod = types.ModuleType("shipfitting.fittingWarnings")
        sys.modules["shipfitting.fittingWarnings"] = fw_mod

        fsvc_mod = types.ModuleType("eve.client.script.environment.fittingSvc")

        class FakeFittingSvc(object):
            pass

        fsvc_mod.fittingSvc = FakeFittingSvc
        sys.modules["eve.client.script.environment.fittingSvc"] = fsvc_mod

        wic_mod = types.ModuleType("eve.client.script.ui.shared.fittingScreen.warningIconCont")

        class PartialWarningIconCont(object):
            def __init__(self, controller):
                self.fittingController = controller
                self.destroyed = False

        wic_mod.WarningIconCont = PartialWarningIconCont
        sys.modules["eve.client.script.ui.shared.fittingScreen.warningIconCont"] = wic_mod

        # Nested import triggers _EvomuMetaHook.find_module -> _install_fitting_active_ship_guard.
        self.assertIsNone(hook.find_module("shipfitting.someNestedDependency"))
        self.assertFalse(self.site._install_fitting_active_ship_guard())
        self.assertNotIn("fittingWarnings", self.site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertNotIn("fittingSvc", self.site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertNotIn("warningIconCont", self.site._HOOKED_CAMERA_GUARD_TARGETS)

        # 3) Module initialization completes and binds EvaluateFit / methods.
        def raw_evaluate_fit(_controller):
            dl.shipIDBeingDisembarked = old_ship
            dl.shipIDBeingEmbarked = new_ship
            builtins.session.shipid = new_ship
            dl.dogmaItems.pop(old_ship, None)
            raise KeyError(old_ship)

        def raw_get_fitting_dict(self):
            return {"ship": builtins.session.shipid}

        def raw_update_icon_thread(self):
            dl.shipIDBeingDisembarked = old_ship
            dl.shipIDBeingEmbarked = new_ship
            builtins.session.shipid = new_ship
            dl.dogmaItems.pop(old_ship, None)
            raise KeyError(old_ship)

        fw_mod.EvaluateFit = raw_evaluate_fit
        FakeFittingSvc.GetFittingDictForActiveShip = raw_get_fitting_dict
        PartialWarningIconCont._WarningIconCont__UpdateIcon_thread = raw_update_icon_thread

        # 4) Execute the deferred uthread callback scheduled in step 1.
        self.assertTrue(scheduled[0]())
        self.assertIn("fittingWarnings", self.site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertIn("fittingSvc", self.site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertIn("warningIconCont", self.site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertTrue(getattr(fw_mod.EvaluateFit, "_evomu_camera_guard", False))
        self.assertTrue(
            getattr(FakeFittingSvc.GetFittingDictForActiveShip, "_evomu_camera_guard", False)
        )
        self.assertTrue(
            getattr(
                PartialWarningIconCont._WarningIconCont__UpdateIcon_thread,
                "_evomu_camera_guard",
                False,
            )
        )

        # Wrapped EvaluateFit and WarningIconCont suppress stale-ship KeyError cleanly.
        self.assertIsNone(fw_mod.EvaluateFit(ctrl))
        dl.dogmaItems = {old_ship: object()}
        dl.shipIDBeingDisembarked = None
        dl.shipIDBeingEmbarked = None
        builtins.session.shipid = old_ship
        wic = PartialWarningIconCont(ctrl)
        self.assertIsNone(wic._WarningIconCont__UpdateIcon_thread())

        # Wrapped fittingSvc.GetFittingDictForActiveShip rejects missing active ship.
        builtins.session.shipid = None
        with self.assertRaises(RuntimeError):
            FakeFittingSvc().GetFittingDictForActiveShip()

        # Already-wrapped callables are recorded without double-wrapping or reporting changed=True.
        wrapped_eval = fw_mod.EvaluateFit
        self.site._HOOKED_CAMERA_GUARD_TARGETS.discard("fittingWarnings")
        self.assertFalse(self.site._install_fitting_active_ship_guard())
        self.assertIn("fittingWarnings", self.site._HOOKED_CAMERA_GUARD_TARGETS)
        self.assertIs(fw_mod.EvaluateFit, wrapped_eval)

    def test_1443_stale_ship_keyerror_fails_closed_when_dogma_location_or_items_missing(self):
        old_ship = 1897898652
        new_ship = 1896955150
        dl, ctrl = self._make_fake_dogma_and_controller(old_ship, new_ship)
        builtins.session = types.SimpleNamespace(shipid=new_ship, stationid=60003760)
        if hasattr(builtins, "sm"):
            delattr(builtins, "sm")

        class BrokenContainer(object):
            def __contains__(self, _key):
                raise RuntimeError("dogmaItems lookup failed")

        # 1) dogmaLocation is None -> fail closed (False).
        ctrl.dogmaLocation = None
        self.assertFalse(
            self.site._is_stale_ship_dogma_key_error(
                KeyError(old_ship),
                controller=ctrl,
                initial_ship_ids=set([old_ship]),
            )
        )

        # 2) dogmaItems is None -> fail closed (False) even when ship swap flags are set.
        ctrl.dogmaLocation = dl
        dl.shipIDBeingDisembarked = old_ship
        dl.shipIDBeingEmbarked = new_ship
        dl.dogmaItems = None
        self.assertFalse(
            self.site._is_stale_ship_dogma_key_error(
                KeyError(old_ship),
                controller=ctrl,
                initial_ship_ids=set([old_ship]),
            )
        )

        # 3) dogmaItems without __contains__ or raising in __contains__ -> fail closed (False).
        for bad_items in (object(), BrokenContainer()):
            dl.dogmaItems = bad_items
            self.assertFalse(
                self.site._is_stale_ship_dogma_key_error(
                    KeyError(old_ship),
                    controller=ctrl,
                    initial_ship_ids=set([old_ship]),
                )
            )

        # 4) End-to-end wrappers MUST re-raise KeyError when dogmaLocation or dogmaItems is None.
        def raise_old_ship(*_args, **_kwargs):
            raise KeyError(old_ship)

        fw_mod, WarningIconCont, StatsPanel, DronePanel = self._register_fitting_modules(
            eval_fn=raise_old_ship,
            icon_fn=raise_old_ship,
            stats_fn=raise_old_ship,
            stats_drone_fn=raise_old_ship,
            drone_panel_fn=raise_old_ship,
        )
        dp = DronePanel(ctrl)
        sp = StatsPanel(ctrl, drone_panel=dp)
        wic = WarningIconCont(ctrl)
        targets = (
            ("EvaluateFit", lambda: fw_mod.EvaluateFit(ctrl)),
            ("WarningIconCont", lambda: wic._WarningIconCont__UpdateIcon_thread()),
            ("StatsPanel.UpdateStats", lambda: sp.UpdateStats()),
            ("DronePanel.UpdateDroneStats", lambda: dp.UpdateDroneStats()),
        )

        ctrl.dogmaLocation = None
        for label, fn in targets:
            with self.subTest(case="dogmaLocation_none", target=label):
                with self.assertRaises(KeyError):
                    fn()

        ctrl.dogmaLocation = dl
        dl.dogmaItems = None
        for label, fn in targets:
            with self.subTest(case="dogmaItems_none", target=label):
                with self.assertRaises(KeyError):
                    fn()


class TestStargateTargetResolution(unittest.TestCase):
    """Regression tests for stargate matching when destination names appear inside 'Stargate' (e.g. 'Tar')."""

    def _make_bp(self):
        return types.SimpleNamespace(
            slimItems={
                50017729: types.SimpleNamespace(
                    groupID=10,
                    categoryID=2,
                    typeID=16,
                    name="Stargate (Kassigainen)",
                    jumps=[types.SimpleNamespace(locationID=30002761)],
                ),
                50017731: types.SimpleNamespace(
                    groupID=10,
                    categoryID=2,
                    typeID=16,
                    name="Stargate (Tarta)",
                    jumps=[types.SimpleNamespace(locationID=30005201)],
                ),
                50017730: types.SimpleNamespace(
                    groupID=10,
                    categoryID=2,
                    typeID=16,
                    name="Stargate (Tar)",
                    jumps=[types.SimpleNamespace(locationID=30005199)],
                ),
            },
            _dists={
                50017729: 1459.0,
                50017731: 1800.0,
                50017730: 2100.0,
            },
            GetBall=lambda bid: types.SimpleNamespace(surfaceDist={50017729: 1459.0, 50017731: 1800.0, 50017730: 2100.0}.get(int(bid), 0.0), mode=2),
            GetSurfaceDist=lambda _sid, bid: {50017729: 1459.0, 50017731: 1800.0, 50017730: 2100.0}.get(int(bid), 0.0),
        )

    def test_tar_does_not_match_stargate_kassigainen_and_prefers_exact_dest_over_prefix(self):
        hook = _load_hook_with_py2_stubs()
        bp = self._make_bp()
        self.assertIsNone(hook._match_stargate_target_tier(bp.slimItems[50017729], "Stargate (Kassigainen)", "tar"))
        self.assertLess(
            hook._match_stargate_target_tier(bp.slimItems[50017730], "Stargate (Tar)", "tar"),
            hook._match_stargate_target_tier(bp.slimItems[50017731], "Stargate (Tarta)", "tar"),
        )
        self.assertIsNotNone(hook._match_stargate_target_tier(bp.slimItems[50017730], "Stargate (Tar)", "tar 0.8"))
        self.assertIsNone(hook._match_stargate_target_tier(bp.slimItems[50017731], "Stargate (Tarta)", "stargate (tar)"))

        # _resolve_space_command_item_id must pick Stargate (Tar) even when Kassigainen/Tarta are closer
        bid, err = hook._resolve_space_command_item_id(bp, 1001, "approach", "Tar")
        self.assertIsNone(err)
        self.assertEqual(bid, 50017730)

        # Generic "stargate" still matches stargates
        bid_generic, err_generic = hook._resolve_space_command_item_id(bp, 1001, "approach", "stargate")
        self.assertIsNone(err_generic)
        self.assertEqual(bid_generic, 50017729)

    def test_execute_jump_gate_and_select_overview_with_tar(self):
        hook = _load_hook_with_py2_stubs()
        helpers = _load_helpers_with_py2_stubs()
        bp = self._make_bp()
        sess = types.SimpleNamespace(
            userid=1, charid=90000001, solarsystemid=30005198, solarsystemid2=30005198,
            stationid=None, structureid=None, shipid=1001, mutating=0, changing=None,
        )
        jumped_gates = []

        def _fake_jump(gate_id):
            jumped_gates.append(int(gate_id))
            dst = 30005199 if int(gate_id) == 50017730 else 30002761
            sess.solarsystemid = dst
            sess.solarsystemid2 = dst

        fake_beyonce = types.SimpleNamespace(
            CmdFollowBall=lambda *_a: None,
            CmdWarpToStuff=lambda *_a: None,
            CmdStargateJump=_fake_jump,
        )
        fake_michelle = types.SimpleNamespace(GetBallpark=lambda: bp)
        fake_state_svc = mock.MagicMock()
        fake_svcs = {"michelle": fake_michelle, "stateSvc": fake_state_svc}
        fake_sm = types.SimpleNamespace(
            services=fake_svcs,
            GetService=lambda n: fake_svcs.get(n),
            RemoteSvc=lambda n: fake_svcs[n],
        )

        # Overview nodes put Kassigainen and Tarta BEFORE exact Tar
        ov_nodes = [
            types.SimpleNamespace(itemID=50017729, slimItem=bp.slimItems[50017729], label="Stargate (Kassigainen)"),
            types.SimpleNamespace(itemID=50017731, slimItem=bp.slimItems[50017731], label="Stargate (Tarta)"),
            types.SimpleNamespace(itemID=50017730, slimItem=bp.slimItems[50017730], label="Stargate (Tar)"),
        ]
        class OverviewWindow:
            pass

        ov_win = OverviewWindow()
        ov_win.windowID = "overview"
        ov_win.destroyed = False
        ov_win.display = True
        ov_win.state = 0
        ov_win.scroll = types.SimpleNamespace(GetNodes=lambda: ov_nodes)
        fake_uicore = types.SimpleNamespace(
            cmd=None,
            desktop=None,
            layer=None,
            registry=types.SimpleNamespace(GetWindows=lambda: [ov_win]),
        )
        snap_base = {"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}

        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_get_state_snapshot", return_value=snap_base), \
             mock.patch.object(hook, "_settle_after_action", return_value=0), \
             mock.patch.object(hook, "_wait_session_settled"), \
             mock.patch.object(hook, "_close_neocom_panels_if_outside"), \
             mock.patch.object(hook, "_close_utilmenu_if_outside"), \
             mock.patch.object(hook, "_close_context_menus_if_outside"), \
             mock.patch.object(hook, "_dismiss_pinned_tooltip_if_outside"), \
             mock.patch.object(hook.H, "_sleep_ms", helpers._sleep_ms, create=True), \
             mock.patch.object(hook.H, "_sleep_sim_ms", lambda _ms: None, create=True), \
             mock.patch.object(hook.H, "_wait_view_settled", lambda _sm: None, create=True), \
             mock.patch.object(hook.H, "_wait_session_timer_clear", lambda max_wait_sec=12.0: None, create=True), \
             mock.patch.object(hook.H, "_get_beyonce", return_value=fake_beyonce, create=True):
            with mock.patch.object(helpers, "_sleep_ms"), mock.patch.object(helpers, "_sleep_sim_ms"):
                res_jump = hook._execute_action(
                    fake_sm,
                    {"type": "jump_gate", "target": "Tar", "wait_ms": 0, "reuse_pre_state": False, "scenario_mode": True, "time_control": False},
                )
                self.assertTrue(res_jump.get("ok"), res_jump)
                self.assertEqual(res_jump.get("jump_gate_id"), 50017730)
                self.assertEqual(res_jump.get("resolved_gate_name"), "Stargate (Tar)")
                self.assertEqual(res_jump.get("to_system_id"), 30005199)
                self.assertEqual(jumped_gates[-1], 50017730)

                # Solar system ID target resolves via slim.jumps[0].locationID instead of failing
                sess.solarsystemid = 30005198
                sess.solarsystemid2 = 30005198
                res_sys = hook._execute_action(
                    fake_sm,
                    {"type": "jump_gate", "target": "30005199", "wait_ms": 0, "reuse_pre_state": False, "scenario_mode": True, "time_control": False},
                )
                self.assertTrue(res_sys.get("ok"), res_sys)
                self.assertEqual(res_sys.get("jump_gate_id"), 50017730)

                # Non-existent gate "Tar" when only Kassigainen is present fails cleanly instead of picking "sTARgate"
                bp.slimItems = {50017729: bp.slimItems[50017729]}
                res_missing = hook._execute_action(
                    fake_sm,
                    {"type": "jump_gate", "target": "Tar", "wait_ms": 0, "reuse_pre_state": False, "scenario_mode": True, "time_control": False},
                )
                self.assertFalse(res_missing.get("ok"), res_missing)
                self.assertIn("no Stargate matching", res_missing.get("error", ""))

                # select_overview("Tar") skips Stargate (Kassigainen) and picks Stargate (Tar)
                bp = self._make_bp()
                res_ov = hook._execute_action(
                    fake_sm,
                    {"type": "select_overview", "target": "Tar", "wait_ms": 0, "reuse_pre_state": False},
                )
                self.assertTrue(res_ov.get("ok"), res_ov)
                self.assertEqual(res_ov.get("selected_item_id"), 50017730)

    def test_inspect_window_content_ignores_faded_out_loading_wheel(self):
        hook = _load_hook_with_py2_stubs()

        class _LoadingWheel:
            def __init__(self, opacity=1.0, parent=None):
                self.name = "loadingWheel"
                self.display = True
                self.state = 1  # UI_DISABLED (as used by CarbonUI LoadingWheel)
                self.destroyed = False
                self.opacity = opacity
                self.parent = parent
                self.children = []

        class _MarketTicker:
            def __init__(self, wheel_opacity=0.0, parent=None, self_opacity=1.0):
                self.name = "MarketTicker"
                self.display = True
                self.state = 0
                self.destroyed = False
                self.opacity = self_opacity
                self.parent = parent
                self.children = [_LoadingWheel(opacity=wheel_opacity, parent=self)]

        class _Label:
            def __init__(self, text, parent=None):
                self.name = "TickerMsg"
                self.display = True
                self.state = 0
                self.destroyed = False
                self.text = text
                self.parent = parent
                self.children = []

        class _RegionalMarket:
            def __init__(self, ticker):
                self.name = "market"
                self.caption = "The Forge Regional Market"
                self.display = True
                self.state = 0
                self.destroyed = False
                self.loading = False
                self.parent = None
                self.children = [ticker, _Label("Best price in Region is: 100 ISK", parent=self)]
                ticker.parent = self

        faded_wnd = _RegionalMarket(_MarketTicker(wheel_opacity=0.0))
        faded_summary = hook._inspect_window_content(faded_wnd)
        self.assertFalse(faded_summary["has_loading"])
        self.assertGreaterEqual(faded_summary["populated_count"], 1)

        faded_parent_wnd = _RegionalMarket(_MarketTicker(wheel_opacity=1.0, self_opacity=0.0))
        self.assertFalse(hook._inspect_window_content(faded_parent_wnd)["has_loading"])

        # Cumulative multiplicative opacity: 0.05 * 0.05 = 0.0025 <= 0.01 must count as faded.
        mult_faded_wnd = _RegionalMarket(_MarketTicker(wheel_opacity=0.05, self_opacity=0.05))
        self.assertFalse(hook._inspect_window_content(mult_faded_wnd)["has_loading"])

        # Ancestor hidden via display=False or state=2 (UI_HIDDEN) rejects direct _is_effectively_opaque calls.
        hidden_display_ticker = _MarketTicker(wheel_opacity=1.0)
        hidden_display_ticker.display = False
        self.assertFalse(hook._is_effectively_opaque(hidden_display_ticker.children[0]))
        hidden_state_ticker = _MarketTicker(wheel_opacity=1.0)
        hidden_state_ticker.state = 2
        self.assertFalse(hook._is_effectively_opaque(hidden_state_ticker.children[0]))

        # Non-finite opacity and property getter exceptions are caught safely without crashing window inspection.
        class _BrokenOpacityWheel(_LoadingWheel):
            @property
            def opacity(self):
                raise RuntimeError("trinity destroyed")

            @opacity.setter
            def opacity(self, _v):
                pass

        broken_ticker = _MarketTicker(wheel_opacity=1.0)
        broken_wheel = _BrokenOpacityWheel(parent=broken_ticker)
        broken_ticker.children = [broken_wheel]
        broken_wnd = _RegionalMarket(broken_ticker)
        self.assertFalse(hook._inspect_window_content(broken_wnd)["has_loading"])

        # Loading text obeys effective opacity too: faded "Loading..." ignored, opaque flagged.
        faded_lbl = _Label("Loading...")
        faded_lbl.opacity = 0.0
        faded_txt_wnd = _RegionalMarket(_MarketTicker(wheel_opacity=0.0))
        faded_lbl.parent = faded_txt_wnd
        faded_txt_wnd.children.append(faded_lbl)
        self.assertFalse(hook._inspect_window_content(faded_txt_wnd)["has_loading"])

        opaque_lbl = _Label("Loading...")
        opaque_lbl.opacity = 1.0
        opaque_txt_wnd = _RegionalMarket(_MarketTicker(wheel_opacity=0.0))
        opaque_lbl.parent = opaque_txt_wnd
        opaque_txt_wnd.children.append(opaque_lbl)
        self.assertTrue(hook._inspect_window_content(opaque_txt_wnd)["has_loading"])

        active_wnd = _RegionalMarket(_MarketTicker(wheel_opacity=1.0))
        self.assertTrue(hook._inspect_window_content(active_wnd)["has_loading"])

    def test_collect_ui_state_resolves_v24_overview_nodes_from_ballpark_and_nearby_fallback(self):
        hook = _load_hook_with_py2_stubs()

        class _OverviewWnd:
            def __init__(self, nodes):
                self.name = "overview"
                self.windowID = "overview"
                self.caption = "Overview"
                self.display = True
                self.state = 0
                self.destroyed = False
                self.scroll = types.SimpleNamespace(GetNodes=lambda: nodes)
                self.children = []

        def _collect_with(nodes, slim_items):
            ov_wnd = _OverviewWnd(nodes)
            bp = types.SimpleNamespace(
                slimItems=slim_items,
                GetBall=lambda bid: types.SimpleNamespace(surfaceDist=1000.0 + float(int(bid) % 500), mode=2),
                GetSurfaceDist=lambda _sid, bid: 1000.0 + float(int(bid) % 500),
            )
            sess = types.SimpleNamespace(
                userid=1, charid=90000001, corpid=1000170,
                regionid=10000002, constellationid=20000020,
                solarsystemid=30000142, solarsystemid2=30000142,
                stationid=None, structureid=None, shipid=100000001, mutating=0,
            )
            fake_sm = types.SimpleNamespace(
                services={"michelle": types.SimpleNamespace(GetBallpark=lambda: bp)},
                GetService=lambda _n: None,
            )
            fake_uicore = types.SimpleNamespace(
                desktop=None,
                registry=types.SimpleNamespace(GetWindows=lambda: [ov_wnd]),
                layer=None,
            )
            with (
                mock.patch.object(hook.__builtin__, "session", sess, create=True),
                mock.patch.object(hook, "_get_uicore", return_value=fake_uicore),
                mock.patch.object(hook, "_ensure_windowed_mode"),
                mock.patch.object(hook, "_install_flight_recorder_hooks"),
                mock.patch.object(hook, "_install_warp_telemetry"),
            ):
                return hook._get_state_snapshot(fake_sm)

        slims_celestials = {
            40009076: types.SimpleNamespace(itemID=40009076, typeID=45041, groupID=6, categoryID=2, name="Jita - Star"),
            60003760: types.SimpleNamespace(itemID=60003760, typeID=1529, groupID=15, categoryID=3, name="Jita IV - Moon 4"),
        }
        # V24 overview nodes without .slimItem / .ball / .distance resolve via bp.slimItems & nearby distances.
        nodes_v24 = [
            types.SimpleNamespace(itemID=40009076, label="Jita - Star"),
            types.SimpleNamespace(itemID=60003760, label="Jita IV - Moon 4"),
        ]
        st_resolved = _collect_with(nodes_v24, slims_celestials)
        ov_items = {e["item_id"]: e for e in st_resolved["space"]["overview_entries"]}
        self.assertEqual(ov_items[40009076]["group_id"], 6)
        self.assertEqual(ov_items[40009076]["category_id"], 2)
        self.assertIsNotNone(ov_items[40009076]["distance_m"])
        self.assertTrue(st_resolved["space"]["celestials_in_overview"])

        # Unresolved overview nodes (group_id=0, e.g. missing from slimItems) do not mask nearby celestials.
        nodes_unresolved = [types.SimpleNamespace(itemID=999999, label="Unknown Object")]
        st_fallback = _collect_with(nodes_unresolved, slims_celestials)
        self.assertEqual(st_fallback["space"]["overview_entries"][0]["group_id"], 0)
        self.assertTrue(st_fallback["space"]["celestials_in_overview"])

        # Resolved non-celestial overview nodes still enforce tab filter (celestials_in_overview=False).
        slims_plus_frigate = dict(slims_celestials)
        slims_plus_frigate[100009999] = types.SimpleNamespace(itemID=100009999, typeID=587, groupID=25, categoryID=6, name="Rifter")
        nodes_pvp_only = [types.SimpleNamespace(itemID=100009999, label="Rifter")]
        st_filtered = _collect_with(nodes_pvp_only, slims_plus_frigate)
        self.assertFalse(st_filtered["space"]["celestials_in_overview"])

        # >22 non-stargate/non-station balls: ballpark distance fallback works even beyond nearby truncation,
        # and malformed overview node IDs (e.g. group headers) do not abort subsequent valid nodes.
        slims_crowded = dict(slims_celestials)
        for idx in range(1, 28):
            bid_planet = 40000100 + idx
            slims_crowded[bid_planet] = types.SimpleNamespace(
                itemID=bid_planet, typeID=11, groupID=7, categoryID=2, name="Planet %d" % idx
            )
        far_planet_id = 40000127
        nodes_crowded = [
            types.SimpleNamespace(itemID="", label="Empty Header"),
            types.SimpleNamespace(itemID="group_ships", label="Ships Header"),
            types.SimpleNamespace(itemID=far_planet_id, groupID="bad", categoryID="bad", label="Planet 27"),
            types.SimpleNamespace(itemID=60003760, label="Jita IV - Moon 4"),
        ]
        st_crowded = _collect_with(nodes_crowded, slims_crowded)
        crowded_by_id = {e["item_id"]: e for e in st_crowded["space"]["overview_entries"]}
        self.assertIn(far_planet_id, crowded_by_id)
        self.assertEqual(crowded_by_id[far_planet_id]["group_id"], 7)
        self.assertIsNotNone(crowded_by_id[far_planet_id]["distance_m"])
        self.assertIn(60003760, crowded_by_id)


class TestIssue1539HostGmTimeControlRouting(unittest.TestCase):
    """#1539: Played client stays role=0; timeControl routes via host GM bridge."""

    def test_no_played_client_timecontrol_calls_in_hook_helpers_driver_or_run_journeys(self):
        import re

        for fname in ("evomu_inclient_driver.py", "evomu_inclient_helpers.py", "evomu_playtester_hook.py"):
            src = (HARNESS_DIR / fname).read_text(encoding="utf-8")
            self.assertEqual(
                re.findall(r'RemoteSvc\(\s*["\']timeControl["\']\s*\)', src),
                [],
                f"{fname} must never invoke RemoteSvc('timeControl') from a played client session",
            )
        rj_src = (HARNESS_DIR / "run_journeys").read_text(encoding="utf-8")
        self.assertNotIn("[[admin_accounts]]", rj_src)
        all_rj_tc = re.findall(r'(\w+)\.RemoteSvc\(\s*["\']timeControl["\']\s*\)', rj_src)
        self.assertGreater(len(all_rj_tc), 0)
        self.assertTrue(all(proxy == "sm_gm" for proxy in all_rj_tc), all_rj_tc)

    def test_helpers_conditional_wait_shortening_on_gm_success_vs_failure(self):
        helpers = _load_helpers_with_py2_stubs()
        sess = types.SimpleNamespace(
            userid=1,
            charid=90000001,
            solarsystemid=30000142,
            solarsystemid2=30000142,
            stationid=None,
            structureid=None,
            shipid=1001,
            mutating=0,
            changing=None,
        )
        fake_bp = types.SimpleNamespace(
            GetBall=lambda _bid: types.SimpleNamespace(mode=2, speed=0.0, maxVelocity=350.0, surfaceDist=1000.0),
            GetSurfaceDist=lambda _a, _b: 1000.0,
            slimItems={},
        )
        fake_beyonce = types.SimpleNamespace(
            CmdWarpToStuff=lambda *_a: None,
            CmdFollowBall=lambda *_a: None,
        )
        fake_ship = types.SimpleNamespace(Undock=lambda: setattr(sess, "stationid", None))
        fake_station = types.SimpleNamespace(
            Dock=lambda sid: (setattr(sess, "stationid", sid), setattr(sess, "solarsystemid", None))
        )
        fake_mich = types.SimpleNamespace(GetBallpark=lambda: fake_bp)
        fake_sm = types.SimpleNamespace(
            services={"michelle": fake_mich, "ship": fake_ship, "stationSvc": fake_station, "beyonce": fake_beyonce},
            GetService=lambda n: fake_sm.services.get(n),
            RemoteSvc=lambda n: fake_sm.services[n],
        )

        # Case 1: _warp_full & _warp_animated drive real _advance_clock through a failing vs succeeding IPC mock
        # (including time=0 truthiness and retry recovery in _ensure_docked_jita / _dock_station ball.mode==3).
        class _GmIpcStub:
            def __init__(self, fail_once=False, ok=True):
                self.ok = ok
                self.fail_once = fail_once
                self.calls = 0

            def request(self, payload, timeout=None):
                self.calls += 1
                cmd = payload.get("cmd")
                if cmd == "advance_clock":
                    if self.fail_once and self.calls == 1:
                        return {"ok": False, "error": "gm_unavailable"}
                    if self.ok:
                        return {"ok": True, "via": "gm_bridge", "target_char_id": payload.get("target_char_id"), "injection_id": f"inj-{self.calls}", "time": 0}
                    return {"ok": False, "error": "gm_unavailable"}
                if cmd == "clear_session_timer":
                    return {"ok": self.ok, "via": "gm_bridge"}
                return {"ok": False, "handled": False}

        for gm_ok, expected_wait in ((True, 4.0), (False, 35.0)):
            waited_timeouts = []
            waited_tcs = []
            warp_calls = [0]
            def _warp_cmd(*_a):
                warp_calls[0] += 1
                if warp_calls[0] == 1:
                    raise RuntimeError("ShipInWarp")
            fake_beyonce.CmdWarpToStuff = _warp_cmd
            ipc_stub = _GmIpcStub(ok=gm_ok)
            with mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm}), \
                 mock.patch.object(helpers, "_try_slash", return_value=None), \
                 mock.patch.object(helpers, "_sleep_ms"), \
                 mock.patch.object(helpers, "_sleep_sim_ms"), \
                 mock.patch.object(helpers, "_wait_view_settled"), \
                 mock.patch.object(helpers, "_wait_warp_fx_settled", side_effect=lambda *_a, **kw: (waited_timeouts.append(kw.get("timeout_sec")), waited_tcs.append(kw.get("allow_time_control")))):
                helpers._warp_full(fake_sm, fake_beyonce, 40009076, ipc=ipc_stub)
            self.assertEqual(waited_timeouts, [expected_wait, expected_wait])
            self.assertEqual(waited_tcs, [gm_ok, gm_ok])

        # _warp_animated fallback wait on GM failure vs success (driving real _advance_clock)
        for gm_ok, expected_anim_wait in ((True, 8.0), (False, 35.0)):
            anim_timeouts = []
            anim_tcs = []
            fake_beyonce.CmdWarpToStuff = lambda *_a: None
            ipc_stub = _GmIpcStub(ok=gm_ok)
            sess_a = types.SimpleNamespace(
                userid=1, charid=90000001, solarsystemid=30000142, solarsystemid2=30000142,
                stationid=None, structureid=None, shipid=1001, mutating=0, changing=None,
            )
            fast_ball = types.SimpleNamespace(mode=3, effectStamp=10, vx=3.0e9, vy=0.0, vz=0.0, surfaceDist=1000.0)
            fast_bp = types.SimpleNamespace(GetBall=lambda _bid: fast_ball, GetSurfaceDist=lambda _a, _b: 1000.0, slimItems={})
            fast_fx = types.SimpleNamespace(balls={1001: {1: types.SimpleNamespace(hasExploded=True, gfx=types.SimpleNamespace(display=True))}})
            fast_sm = types.SimpleNamespace(
                services={"michelle": types.SimpleNamespace(GetBallpark=lambda: fast_bp), "FxSequencer": fast_fx, "beyonce": fake_beyonce},
                GetService=lambda n: fast_sm.services.get(n),
                RemoteSvc=lambda n: fast_sm.services[n],
            )
            with mock.patch.dict(builtins.__dict__, {"session": sess_a, "sm": fast_sm}), \
                 mock.patch.object(helpers, "_try_slash", return_value=None), \
                 mock.patch.object(helpers, "_ensure_warp_signal_tracker"), \
                 mock.patch.object(helpers, "_sleep_ms"), \
                 mock.patch.object(helpers, "_sleep_sim_ms"), \
                 mock.patch.object(helpers, "_wait_view_settled"), \
                 mock.patch.dict(helpers._WARP_SIGNAL_STATE, {"hooked": False, "started": 0, "finished": 0}), \
                 mock.patch.object(helpers, "_wait_warp_fx_settled", side_effect=lambda *_a, **kw: (anim_timeouts.append(kw.get("timeout_sec")), anim_tcs.append(kw.get("allow_time_control")))):
                helpers._warp_animated(fast_sm, fake_beyonce, 40009076, max_ticks=1, ipc=ipc_stub)
            self.assertEqual(anim_timeouts, [expected_anim_wait])
            self.assertEqual(anim_tcs, [gm_ok])

        # _dock_station with active warp (ball.mode == 3) waits for GM AdvanceClock instead of blindly using 6.0s
        for gm_ok, expected_pre_wait in ((True, 6.0), (False, 35.0)):
            pre_dock_waits = []
            pre_dock_tcs = []
            ipc_stub = _GmIpcStub(ok=gm_ok)
            warping_bp_d = types.SimpleNamespace(GetBall=lambda _bid: types.SimpleNamespace(mode=3, surfaceDist=1000.0))
            sess.stationid = None
            sess.solarsystemid = 30000142
            fake_sm_w_d = types.SimpleNamespace(
                services={
                    "michelle": types.SimpleNamespace(GetBallpark=lambda: warping_bp_d),
                    "stationSvc": types.SimpleNamespace(Dock=lambda sid: (setattr(sess, "stationid", sid), setattr(sess, "solarsystemid", None))),
                    "beyonce": fake_beyonce,
                },
                GetService=lambda n: fake_sm_w_d.services.get(n),
                RemoteSvc=lambda n: fake_sm_w_d.services[n],
            )
            with mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm_w_d}), \
                 mock.patch.object(helpers, "_wait_session_timer_clear"), \
                 mock.patch.object(helpers, "_wait_view_settled"), \
                 mock.patch.object(helpers, "_ensure_view"), \
                 mock.patch.object(helpers, "_wait_warp_fx_settled", side_effect=lambda *_a, **kw: (pre_dock_waits.append(kw.get("timeout_sec")), pre_dock_tcs.append(kw.get("allow_time_control")))):
                helpers._dock_station(fake_sm_w_d, 60003760, ipc=ipc_stub, allow_time_control=True)
            self.assertEqual(pre_dock_waits, [expected_pre_wait])
            self.assertEqual(pre_dock_tcs, [gm_ok])

        # _ensure_docked_jita propagates allow_time_control=bool(adv_ok) and refreshes adv_ok on retry
        sess_j = types.SimpleNamespace(
            userid=1, charid=90000001, solarsystemid=30000142, solarsystemid2=30000142,
            stationid=None, structureid=None, shipid=1001, mutating=0, changing=None,
        )
        dock_tries = [0]
        def _retry_dock(sid):
            dock_tries[0] += 1
            if dock_tries[0] == 1:
                raise RuntimeError("OutsideDockingPerimeter")
            sess_j.stationid = sid
            sess_j.solarsystemid = None
        seen_jita_tcs = []
        ipc_retry = _GmIpcStub(fail_once=True, ok=True)
        fake_sm_j = types.SimpleNamespace(
            services={
                "michelle": fake_mich,
                "stationSvc": types.SimpleNamespace(Dock=_retry_dock),
                "beyonce": types.SimpleNamespace(MachoBindObject=lambda *_a: (types.SimpleNamespace(CmdStop=lambda: None), None)),
            },
            GetService=lambda n: fake_sm_j.services.get(n),
            RemoteSvc=lambda n: fake_sm_j.services[n],
        )
        with mock.patch.dict(builtins.__dict__, {"session": sess_j, "sm": fake_sm_j}), \
             mock.patch.object(helpers, "_try_slash", return_value=None), \
             mock.patch.object(helpers, "_wait_session_attr"), \
             mock.patch.object(helpers, "_wait_view_settled"), \
             mock.patch.object(helpers, "_ensure_view"), \
             mock.patch.object(helpers, "_sleep_ms"), \
             mock.patch.object(helpers, "_sleep_sim_ms"), \
             mock.patch.object(helpers, "_wait_warp_fx_settled", side_effect=lambda *_a, **kw: seen_jita_tcs.append(kw.get("allow_time_control"))):
            helpers._ensure_docked_jita(fake_sm_j, ipc_retry, station_id=60003760, system_id=30000142)
        self.assertEqual(dock_tries[0], 2)
        self.assertEqual(ipc_retry.calls, 3)
        self.assertEqual(seen_jita_tcs, [False, False])

        sess_j_ok = types.SimpleNamespace(
            userid=1, charid=90000001, solarsystemid=30000142, solarsystemid2=30000142,
            stationid=None, structureid=None, shipid=1001, mutating=0, changing=None,
        )
        dock_tries_ok = [0]
        def _retry_dock_ok(sid):
            dock_tries_ok[0] += 1
            if dock_tries_ok[0] == 1:
                raise RuntimeError("OutsideDockingPerimeter")
            sess_j_ok.stationid = sid
            sess_j_ok.solarsystemid = None
        seen_jita_ok_tcs = []
        ipc_ok = _GmIpcStub(fail_once=False, ok=True)
        fake_sm_j_ok = types.SimpleNamespace(
            services={
                "michelle": fake_mich,
                "stationSvc": types.SimpleNamespace(Dock=_retry_dock_ok),
                "beyonce": types.SimpleNamespace(MachoBindObject=lambda *_a: (types.SimpleNamespace(CmdStop=lambda: None), None)),
            },
            GetService=lambda n: fake_sm_j_ok.services.get(n),
            RemoteSvc=lambda n: fake_sm_j_ok.services[n],
        )
        with mock.patch.dict(builtins.__dict__, {"session": sess_j_ok, "sm": fake_sm_j_ok}), \
             mock.patch.object(helpers, "_try_slash", return_value=None), \
             mock.patch.object(helpers, "_wait_session_attr"), \
             mock.patch.object(helpers, "_wait_view_settled"), \
             mock.patch.object(helpers, "_ensure_view"), \
             mock.patch.object(helpers, "_sleep_ms"), \
             mock.patch.object(helpers, "_sleep_sim_ms"), \
             mock.patch.object(helpers, "_wait_warp_fx_settled", side_effect=lambda *_a, **kw: seen_jita_ok_tcs.append(kw.get("allow_time_control"))):
            helpers._ensure_docked_jita(fake_sm_j_ok, ipc_ok, station_id=60003760, system_id=30000142)
        self.assertEqual(dock_tries_ok[0], 2)
        self.assertEqual(seen_jita_ok_tcs, [False, True])

        # Check direct fallback timeout scaling inside _wait_warp_fx_settled when warp is active.
        warp_clock = {"t": 100.0}
        def _advance_warp_t():
            warp_clock["t"] += 0.5
            return warp_clock["t"]

        seen_loops = {}
        for gm_ok in (True, False):
            warp_clock["t"] = 100.0
            seen_balls = []
            sess_w = types.SimpleNamespace(
                userid=1, charid=90000001, solarsystemid=30000142, solarsystemid2=30000142,
                stationid=None, structureid=None, shipid=1001, mutating=0, changing=None,
            )
            warping_bp = types.SimpleNamespace(
                GetBall=lambda _bid: (seen_balls.append(warp_clock["t"]), types.SimpleNamespace(mode=3))[1]
            )
            fake_mich_w = types.SimpleNamespace(GetBallpark=lambda: warping_bp)
            fake_sm_w = types.SimpleNamespace(
                services={"michelle": fake_mich_w},
                GetService=lambda n: fake_sm_w.services.get(n),
                RemoteSvc=lambda n: fake_sm_w.services[n],
            )
            explicit_ipc_w = object()
            adv_ipcs_w = []
            with mock.patch.dict(builtins.__dict__, {"session": sess_w, "sm": fake_sm_w}), \
                 mock.patch.object(helpers, "_advance_clock", side_effect=lambda _s, ipc_arg, *_a, **_kw: (adv_ipcs_w.append(ipc_arg), gm_ok)[1]), \
                 mock.patch.object(helpers, "_ensure_warp_signal_tracker", return_value=None), \
                 mock.patch.object(helpers, "_sleep_sim_ms"), \
                 mock.patch.object(helpers, "_sleep_ms"), \
                 mock.patch.object(helpers.time, "time", side_effect=_advance_warp_t):
                helpers._wait_warp_fx_settled(fake_sm_w, timeout_sec=1.0, allow_time_control=True, ipc=explicit_ipc_w)
            self.assertEqual(adv_ipcs_w, [explicit_ipc_w])
            seen_loops[gm_ok] = seen_balls[-1] - 100.0
        self.assertLessEqual(seen_loops[True], 6.0)
        self.assertGreaterEqual(seen_loops[False], 12.0)

        # Case 2: _undock_ship & _dock_station use 10.0s vs 16.0s wait budget on GM success vs failure,
        # and only fall back to _wait_session_timer_clear(max_wait_sec=12.0) when _clear_session_timer fails.
        for gm_ok, expected_wait in ((True, 10.0), (False, 16.0)):
            sim_t = [100.0]
            op_start = [100.0]
            timer_clear_waits = []

            def _sleep_step(_ms):
                sim_t[0] += 0.5

            def _mock_undock():
                op_start[0] = sim_t[0]

            def _mock_dock(_sid):
                op_start[0] = sim_t[0]

            fake_sm_dt = types.SimpleNamespace(
                services={
                    "michelle": fake_mich,
                    "ship": types.SimpleNamespace(Undock=_mock_undock),
                    "stationSvc": types.SimpleNamespace(Dock=_mock_dock),
                    "beyonce": fake_beyonce,
                },
                GetService=lambda n: fake_sm_dt.services.get(n),
                RemoteSvc=lambda n: fake_sm_dt.services[n],
            )

            sess.stationid = 60003760
            sess.solarsystemid = None
            with mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm_dt}), \
                 mock.patch.object(helpers, "_clear_session_timer", return_value=gm_ok), \
                 mock.patch.object(helpers, "_wait_session_timer_clear", side_effect=lambda **kw: timer_clear_waits.append(kw.get("max_wait_sec"))), \
                 mock.patch.object(helpers, "_wait_view_settled"), \
                 mock.patch.object(helpers, "_ensure_view"), \
                 mock.patch.object(helpers, "_sleep_ms", side_effect=_sleep_step), \
                 mock.patch.object(helpers.time, "time", side_effect=lambda: sim_t[0]):
                with self.assertRaises(RuntimeError):
                    helpers._undock_ship(fake_sm_dt, ipc=None, allow_time_control=True)
            self.assertEqual(sim_t[0] - op_start[0], expected_wait)

            sess.stationid = None
            sess.solarsystemid = 30000142
            with mock.patch.dict(builtins.__dict__, {"session": sess, "sm": fake_sm_dt}), \
                 mock.patch.object(helpers, "_clear_session_timer", return_value=gm_ok), \
                 mock.patch.object(helpers, "_wait_session_timer_clear", side_effect=lambda **kw: timer_clear_waits.append(kw.get("max_wait_sec"))), \
                 mock.patch.object(helpers, "_wait_warp_fx_settled"), \
                 mock.patch.object(helpers, "_wait_view_settled"), \
                 mock.patch.object(helpers, "_ensure_view"), \
                 mock.patch.object(helpers, "_sleep_ms", side_effect=_sleep_step), \
                 mock.patch.object(helpers.time, "time", side_effect=lambda: sim_t[0]):
                with self.assertRaises(RuntimeError):
                    helpers._dock_station(fake_sm_dt, 60003760, ipc=None, allow_time_control=True)
            self.assertEqual(sim_t[0] - op_start[0], expected_wait)
            self.assertEqual(timer_clear_waits, [] if gm_ok else [12.0, 12.0])

    def test_hook_dock_and_jump_gate_conditional_wait_budgets_and_sim_telemetry(self):
        hook = _load_hook_with_py2_stubs()
        real_helpers = _load_helpers_with_py2_stubs()
        hook.H = real_helpers
        real_helpers._reset_sim_time_telemetry()

        # Verify hook._execute_action uses shortened budgets on GM success vs full budgets on GM failure
        seen_settle_timeouts = {"dock_true": [], "dock_false": [], "jump_true": [], "jump_false": []}
        dock_wait_spans = {}
        jump_wait_spans = {}
        for gm_ok, suffix in ((True, "true"), (False, "false")):
            sess = types.SimpleNamespace(
                userid=1, charid=90000001, solarsystemid=30000142, solarsystemid2=30000142,
                stationid=None, structureid=None, shipid=2001, mutating=0, changing=None,
            )
            dist = {"sd": 200000.0}
            fake_bp = types.SimpleNamespace(
                slimItems={60003760: types.SimpleNamespace(categoryID=3), 50001234: types.SimpleNamespace(groupID=10)},
                GetBall=lambda bid: types.SimpleNamespace(mode=2, surfaceDist=dist["sd"]),
                GetSurfaceDist=lambda _a, _b: dist["sd"],
            )
            def _on_warp(*_a):
                dist["sd"] = 1000.0
            def _on_dock(sid):
                # Keep stationid unset so dock exercises the dl_d (10s vs 16s) poll deadline
                pass
            def _on_jump(_gid):
                # Keep solarsystemid unchanged so jump exercises jump_dl (12s vs 18s)
                pass
            fake_beyonce = types.SimpleNamespace(
                CmdFollowBall=lambda *_a: None,
                CmdWarpToStuff=_on_warp,
                CmdStargateJump=_on_jump,
            )
            fake_station = types.SimpleNamespace(Dock=_on_dock)
            fake_mich = types.SimpleNamespace(GetBallpark=lambda: fake_bp)
            fake_sm = mock.Mock()
            fake_sm.IsServiceRunning.return_value = False
            fake_sm.services = {"michelle": fake_mich, "stationSvc": fake_station}
            fake_sm.GetService.side_effect = lambda n: {"michelle": fake_mich, "viewState": types.SimpleNamespace(secondaryInfo=None)}.get(n)
            fake_sm.RemoteSvc.side_effect = lambda n: {"stationSvc": fake_station}[n]
            fake_es = types.SimpleNamespace(_EXC_SEQ=0, _RECENT_EXCEPTIONS=[], _PT_CONSUMED_EXC_SEQ=0, _REPORTED_LOG_EXC_SIGS=set())

            class _FakeGmClient:
                def __init__(self, allow):
                    self.allow = allow
                    self.seq = 0
                def request(self, payload):
                    if not self.allow:
                        return {"ok": False, "reason": "gm_unavailable"}
                    cmd = payload.get("cmd")
                    if cmd == "clear_session_timer":
                        return {"ok": True, "via": "gm_bridge", "target_char_id": payload.get("target_char_id")}
                    if cmd == "advance_clock":
                        self.seq += 1
                        return {
                            "ok": True,
                            "via": "gm_bridge",
                            "target_char_id": payload.get("target_char_id"),
                            "injection_id": f"inj-{suffix}-{self.seq}",
                        }
                    return {"ok": False}

            fake_ipc = _FakeGmClient(gm_ok)
            real_helpers._configure_host_gm_bridge(ipc=fake_ipc, target_char_id=90000001)
            t_clock = {"t": 1000.0, "dock_done": False, "jump_done": False}
            def _fake_time():
                t_clock["t"] += 1.0
                return t_clock["t"]
            def _fake_sleep_ms(_ms):
                pass
            def _mark_dock_done(_sm, _v):
                t_clock["dock_done"] = True
            with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
                 mock.patch.object(builtins, "session", sess, create=True), \
                 mock.patch.object(real_helpers.__builtin__, "session", sess, create=True), \
                 mock.patch.object(hook.time, "time", side_effect=_fake_time), \
                 mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                 mock.patch.object(hook, "_install_warp_telemetry"), \
                 mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
                 mock.patch.object(real_helpers, "_get_beyonce", return_value=fake_beyonce), \
                 mock.patch.object(real_helpers, "_wait_warp_fx_settled", side_effect=lambda _s, timeout_sec=12.0, **_kw: seen_settle_timeouts[f"dock_{suffix}"].append(timeout_sec)), \
                 mock.patch.object(real_helpers, "_wait_session_timer_clear", return_value=None), \
                 mock.patch.object(real_helpers, "_wait_view_settled"), \
                 mock.patch.object(real_helpers, "_ensure_view", side_effect=_mark_dock_done), \
                 mock.patch.object(real_helpers, "_sleep_ms", side_effect=_fake_sleep_ms), \
                 mock.patch.object(real_helpers, "_sleep_sim_ms"):
                res_dock = hook._execute_action(fake_sm, {"type": "dock", "target": "60003760"})
            self.assertTrue(res_dock.get("ok"), res_dock)
            dock_wait_spans[gm_ok] = t_clock["t"] - 1000.0

            dist["sd"] = 200000.0
            t_clock["t"] = 2000.0
            sess.stationid = None
            sess.solarsystemid = 30000142
            sess.solarsystemid2 = 30000142
            with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
                 mock.patch.object(builtins, "session", sess, create=True), \
                 mock.patch.object(real_helpers.__builtin__, "session", sess, create=True), \
                 mock.patch.object(hook.time, "time", side_effect=_fake_time), \
                 mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                 mock.patch.object(hook, "_install_warp_telemetry"), \
                 mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
                 mock.patch.object(real_helpers, "_get_beyonce", return_value=fake_beyonce), \
                 mock.patch.object(real_helpers, "_wait_warp_fx_settled", side_effect=lambda _s, timeout_sec=12.0, **_kw: seen_settle_timeouts[f"jump_{suffix}"].append(timeout_sec)), \
                 mock.patch.object(real_helpers, "_wait_session_timer_clear", return_value=None), \
                 mock.patch.object(real_helpers, "_wait_view_settled"), \
                 mock.patch.object(real_helpers, "_ensure_view"), \
                 mock.patch.object(real_helpers, "_sleep_ms", side_effect=_fake_sleep_ms), \
                 mock.patch.object(real_helpers, "_sleep_sim_ms"):
                res_jump = hook._execute_action(fake_sm, {"type": "jump_gate", "target": "50001234", "item_id": 50001234})
            jump_wait_spans[gm_ok] = t_clock["t"] - 2000.0
            self.assertEqual(res_jump.get("to_system_id"), 30000142)
            if gm_ok:
                # 180s warp advance for jump_gate only (cursor resets per action; no double-count from prior dock step)
                self.assertEqual(res_jump.get("sim_time_injected_sec"), 180.0)
                self.assertEqual(res_jump.get("sim_time_injection_count"), 1)
                self.assertEqual(
                    res_jump["flight_recorder"]["sim_time_injections"][0]["injection_id"],
                    f"inj-{suffix}-2",
                )
            else:
                self.assertEqual(res_jump.get("sim_time_injected_sec"), 0.0)
                self.assertEqual(res_jump.get("sim_time_injection_count"), 0)

        self.assertEqual(seen_settle_timeouts["dock_true"], [8.0])
        self.assertEqual(seen_settle_timeouts["dock_false"], [35.0])
        self.assertEqual(seen_settle_timeouts["jump_true"], [8.0])
        self.assertEqual(seen_settle_timeouts["jump_false"], [35.0])
        self.assertLess(dock_wait_spans[True], dock_wait_spans[False])
        self.assertLess(jump_wait_spans[True], jump_wait_spans[False])

    def test_hook_jump_gate_within_150km_no_unbound_local_error(self):
        hook = _load_hook_with_py2_stubs()
        real_helpers = _load_helpers_with_py2_stubs()
        hook.H = real_helpers
        real_helpers._reset_sim_time_telemetry()

        for allow_tc in (True, False):
            sess = types.SimpleNamespace(
                userid=1, charid=90000001, solarsystemid=30000142, solarsystemid2=30000142,
                stationid=None, structureid=None, shipid=2001, mutating=0, changing=None,
            )
            dist = {"sd": 10000.0}
            follow_calls = []
            warp_calls = []
            jump_calls = []
            fake_bp = types.SimpleNamespace(
                slimItems={50001234: types.SimpleNamespace(groupID=10, name="Stargate (Perimeter)")},
                GetBall=lambda _bid: types.SimpleNamespace(mode=2, surfaceDist=dist["sd"]),
                GetSurfaceDist=lambda _a, _b: dist["sd"],
            )
            def _on_follow(_gid, _range):
                follow_calls.append(_gid)
                dist["sd"] = 1000.0
            fake_beyonce = types.SimpleNamespace(
                CmdWarpToStuff=lambda *_a: warp_calls.append(_a),
                CmdFollowBall=_on_follow,
                CmdStargateJump=lambda gate_id: (jump_calls.append(gate_id), setattr(sess, "solarsystemid2", 30000144)),
            )
            fake_map = types.SimpleNamespace(GetSolarsystemItems=lambda _s, *_a: [], GetDestinationPath=lambda *_a: [])
            fake_sm = types.SimpleNamespace(
                services={
                    "michelle": types.SimpleNamespace(GetBallpark=lambda: fake_bp),
                    "map": fake_map,
                    "beyonce": fake_beyonce,
                },
                GetService=lambda n: fake_sm.services.get(n),
                RemoteSvc=lambda n: fake_sm.services[n],
            )
            ipc_calls = []
            def _ipc_req(payload, timeout=None):
                ipc_calls.append(dict(payload))
                if allow_tc:
                    return {"ok": True, "via": "gm_bridge", "target_char_id": 90000001, "injection_id": "inj-10km", "time": 133000000}
                return {"ok": False, "reason": "host_time_control_disabled"}
            ipc_stub = types.SimpleNamespace(request=_ipc_req)
            fake_es = types.SimpleNamespace(_read_guard_hits=lambda: [], _wrap_macho_rpc_hooks=lambda *_a: None)
            t_clock = {"t": 3000.0}
            with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
                 mock.patch.object(real_helpers, "_HOST_GM_STATE", {"port": 0, "client": ipc_stub, "target_char_id": 90000001, "sim_seq": 0, "sim_events": [], "configured_with_ipc": True, "mode": "journeys"}), \
                 mock.patch.object(builtins, "session", sess, create=True), \
                 mock.patch.object(real_helpers.__builtin__, "session", sess, create=True), \
                 mock.patch.object(hook.time, "time", side_effect=lambda: (t_clock.__setitem__("t", t_clock["t"] + 0.1), t_clock["t"])[1]), \
                 mock.patch.object(hook, "_install_flight_recorder_hooks"), \
                 mock.patch.object(hook, "_install_warp_telemetry"), \
                 mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
                 mock.patch.object(real_helpers, "_get_beyonce", return_value=fake_beyonce), \
                 mock.patch.object(real_helpers, "_wait_session_timer_clear", return_value=None), \
                 mock.patch.object(real_helpers, "_wait_view_settled"), \
                 mock.patch.object(real_helpers, "_ensure_view"), \
                 mock.patch.object(real_helpers, "_sleep_ms"), \
                 mock.patch.object(real_helpers, "_sleep_sim_ms"):
                res_jump = hook._execute_action(
                    fake_sm,
                    {"type": "jump_gate", "target": "50001234", "item_id": 50001234, "time_control": allow_tc},
                )
            self.assertEqual(warp_calls, [])
            self.assertEqual(follow_calls, [50001234])
            self.assertEqual(jump_calls, [50001234])
            self.assertTrue(res_jump.get("ok"), res_jump)
            if allow_tc:
                self.assertGreaterEqual(len(ipc_calls), 1)
            else:
                self.assertEqual(ipc_calls, [])

    def test_open_structure_cosmetics_run_slash_and_try_slash_fail_closed_without_client_slash(self):
        hook = _load_hook_with_py2_stubs()
        real_helpers = _load_helpers_with_py2_stubs()
        hook.H = real_helpers

        sess = types.SimpleNamespace(
            userid=1, charid=90000001, corpid=1000167, corprole=0,
            solarsystemid=None, solarsystemid2=30000142, stationid=60003760,
            structureid=None, shipid=1001, mutating=0, changing=None,
        )
        client_slash_calls = []
        ipc_slash_calls = []

        class _PlayedSlashSvc:
            remoteCommandList = ["heal"]
            def SlashCmd(self, cmd):
                client_slash_calls.append(cmd)
                raise AssertionError("played client slash.SlashCmd must never be called: %r" % (cmd,))

        fake_corp = types.SimpleNamespace(AddCorporation=lambda name, ticker: setattr(sess, "corpid", 98000001))
        fake_ctrl = types.SimpleNamespace(get_cosmetic_state=lambda *_a, **_kw: {"ok": True})
        fake_cos = types.SimpleNamespace(_structure_cosmetic_states_controller=fake_ctrl)
        fake_lic = types.SimpleNamespace(get_structure_licenses_for_corporation=lambda: [])
        fake_sm = types.SimpleNamespace(
            services={
                "slash": _PlayedSlashSvc(),
                "corpRegistry": fake_corp,
                "cosmeticsSvc": fake_cos,
                "cosmeticsLicenseSvc": fake_lic,
                "loyaltyPointsWalletSvc": types.SimpleNamespace(_corpLPBalanceByIssuerCorpID={}),
            },
            GetService=lambda n: fake_sm.services.get(n),
            RemoteSvc=lambda n: fake_sm.services[n],
        )

        ipc_ok = types.SimpleNamespace(
            request=lambda payload, timeout=None: (ipc_slash_calls.append(dict(payload)), {"ok": True, "handled": True, "via": "gm_bridge", "result": {"ok": True}})[1]
        )
        fake_es = types.SimpleNamespace(_read_guard_hits=lambda: [], _wrap_macho_rpc_hooks=lambda *_a: None)
        with mock.patch.dict(sys.modules, {"evomu_site": fake_es}), \
             mock.patch.object(real_helpers, "_HOST_GM_STATE", {"port": 0, "client": ipc_ok, "target_char_id": 90000001, "sim_seq": 0, "sim_events": [], "configured_with_ipc": True, "mode": "playtester"}), \
             mock.patch.object(builtins, "session", sess, create=True), \
             mock.patch.object(real_helpers.__builtin__, "session", sess, create=True), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(real_helpers, "_sync_quasar_gateway"), \
             mock.patch.object(real_helpers, "_sleep_ms"):
            res_cos = hook._execute_action(fake_sm, {"type": "open_structure_cosmetics", "target": "1000000000001"})
            self.assertTrue(res_cos.get("ok"), res_cos)
            res_rs = hook._execute_action(fake_sm, {"type": "run_slash", "slash_cmd": "/heal"})
            self.assertTrue(res_rs.get("ok"), res_rs)

        self.assertEqual(client_slash_calls, [])
        self.assertEqual([c["command"] for c in ipc_slash_calls], ["/cleartimers", "/giveskill 3363 1", "/heal"])

        # Host-returned slash sim_time_injected_sec/injection_id mirrors into client-side collect_sim_time_telemetry.
        sim_slash_ipc = types.SimpleNamespace(
            request=lambda payload, timeout=None: {
                "ok": True,
                "handled": True,
                "via": "gm_bridge",
                "command": payload["command"],
                "target_char_id": 90000001,
                "injection_id": "host-sim-slash-1",
                "sim_time_injected_sec": 600.0,
                "result": {"advancedSeconds": 600},
            }
        )
        with mock.patch.object(real_helpers, "_HOST_GM_STATE", {"port": 0, "client": sim_slash_ipc, "target_char_id": 90000001, "sim_seq": 0, "sim_events": [], "configured_with_ipc": True, "mode": "playtester"}):
            real_helpers._reset_sim_time_telemetry()
            real_helpers._try_slash(fake_sm, "/advancetime 600", raise_on_error=True)
            client_sim_tel = real_helpers.collect_sim_time_telemetry()
            self.assertAlmostEqual(client_sim_tel["sim_time_injected_sec"], 600.0)
            self.assertEqual(client_sim_tel["sim_time_injection_count"], 1)
            self.assertEqual(client_sim_tel["sim_time_injections"][0]["injection_id"], "host-sim-slash-1")

        # Host denial (even with handled=False / reason present) fails closed with zero played-client slash.SlashCmd fallback.
        denied_ipc = types.SimpleNamespace(
            request=lambda payload, timeout=None: {"ok": False, "handled": True, "reason": "host_gm_slash_disabled"}
        )
        with mock.patch.object(real_helpers, "_HOST_GM_STATE", {"port": 0, "client": denied_ipc, "target_char_id": 90000001, "sim_seq": 0, "sim_events": [], "configured_with_ipc": True, "mode": "scenario"}):
            self.assertIsNone(real_helpers._try_slash(fake_sm, "/heal", raise_on_error=False))
            with self.assertRaises(RuntimeError):
                real_helpers._try_slash(fake_sm, "/heal", raise_on_error=True)

        # EVOMU_HOST_GM_PORT never leaks into run_journeys ipc mode or explicit host_gm_port=0 scenario config.
        with mock.patch.dict(real_helpers.os.environ, {"EVOMU_HOST_GM_PORT": "59999"}):
            st_ipc = real_helpers._configure_host_gm_bridge(cfg=None, ipc=denied_ipc, target_char_id=90000001)
            self.assertEqual(st_ipc["port"], 0)
            st_scen = real_helpers._configure_host_gm_bridge(cfg={"mode": "scenario", "host_gm_port": 0, "char_id": 90000001}, ipc=None)
            self.assertEqual(st_scen["port"], 0)
            self.assertIsNone(real_helpers._try_slash(fake_sm, "/heal", raise_on_error=False))
        self.assertEqual(client_slash_calls, [])


class TestHarnessSettleAndDeepWindowInspect1190(unittest.TestCase):
    def test_inspect_window_content_handles_deep_skillplanner_hierarchy(self):
        hook = _load_hook_with_py2_stubs()

        class _Node(object):
            def __init__(self, name="Container", text=None, children=None):
                self.name = name
                self.display = True
                self.state = 0
                self.destroyed = False
                self.text = text
                self.children = list(children or [])

        # Populate >120 leading sibling containers and place SkillPlanner tabs/labels at index 30, depth 11.
        leading = [_Node(name=f"filler_{i}") for i in range(135)]
        deep_leaf = _Node(name="SkillEntry", text="Spaceship Command")
        cur = deep_leaf
        for lvl in range(10):
            cur = _Node(name=f"skill_nested_{lvl}", children=[cur])
        wide_siblings = [_Node(name=f"slot_{i}") for i in range(32)]
        wide_siblings[30] = cur
        root = _Node(name="SkillWindow", children=leading[:35] + [_Node(name="mainCont", children=wide_siblings)])
        root.caption = "Skills"
        root.loading = False

        summary = hook._inspect_window_content(root)
        self.assertGreaterEqual(summary["populated_count"], 1, summary)
        self.assertGreaterEqual(summary["text_count"], 1, summary)
        self.assertIn("Spaceship Command", summary["sample_texts"])

    def test_space_command_maneuver_honors_explicit_wait_ms_over_600(self):
        hook = _load_hook_with_py2_stubs()
        import builtins

        sess = types.SimpleNamespace(
            solarsystemid=30000142,
            solarsystemid2=30000142,
            stationid=None,
            structureid=None,
            shipid=90010001,
        )
        beyonce = mock.MagicMock()
        ballpark = types.SimpleNamespace(balls={60003760: object(), 90010001: object()})
        michelle = types.SimpleNamespace(GetBallpark=lambda: ballpark)
        view_state = types.SimpleNamespace(secondaryInfo=None)
        fake_sm = mock.MagicMock()
        fake_sm.GetService.side_effect = lambda name: {
            "viewState": view_state,
            "michelle": michelle,
        }.get(name, mock.MagicMock())
        fake_uicore = types.SimpleNamespace(cmd=None, registry=None, layer=None, desktop=None)
        sleep_calls = []
        settle_calls = []

        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(builtins, "session", sess, create=True), \
             mock.patch.object(hook, "__builtin__", types.SimpleNamespace(session=sess)), \
             mock.patch.object(hook.H, "_get_beyonce", create=True, return_value=beyonce), \
             mock.patch.object(hook.H, "_sleep_ms", create=True, side_effect=lambda ms: sleep_calls.append(ms)), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_wait_session_settled"), \
             mock.patch.object(hook, "_sample_ego_warp_state", return_value={"mode": 1, "speed_mps": 45.0}), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {"ego": {"mode": 1, "speed_mps": 45.0}}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
             mock.patch.object(hook, "_settle_after_action", side_effect=lambda *a, **kw: settle_calls.append(kw) or 100):
            res = hook._execute_action(
                fake_sm,
                {"type": "approach", "target": "60003760", "item_id": 60003760, "wait_ms": 3500},
            )

        self.assertTrue(res.get("ok"), res)
        beyonce.CmdFollowBall.assert_called_once_with(60003760, 50.0)
        self.assertIn(3500, sleep_calls)
        self.assertEqual(res.get("waited_ms"), 3500)
        self.assertEqual(settle_calls, [])

    def test_ui_click_under_5000ms_keeps_adaptive_600ms_settle(self):
        hook = _load_hook_with_py2_stubs()

        class _Btn(object):
            name = "closeBtn"
            def OnClick(self, *args):
                return None

        fake_uicore = types.SimpleNamespace(cmd=None, registry=None, layer=None, desktop=None)
        fake_sm = mock.MagicMock()
        captured_settle = []
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_find_widget", return_value=_Btn()), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "hangar"}), \
             mock.patch.object(hook, "_settle_after_action", side_effect=lambda *a, **kw: captured_settle.append(kw) or 150):
            res = hook._execute_action(
                fake_sm,
                {"type": "click_widget", "target": "closeBtn", "wait_ms": 3500},
            )

        self.assertTrue(res.get("ok"), res)
        self.assertEqual(len(captured_settle), 1)
        self.assertEqual(captured_settle[0]["max_ms"], 600)

    def test_save_location_writes_v24_descendant_textfield_or_falls_back_cleanly(self):
        hook = _load_hook_with_py2_stubs()
        import builtins

        replaced = []
        cleared = []
        confirmed = []
        closed = []
        fallback_calls = []

        class _Controller(object):
            text = "spot in Jita solar system"
            def clear_text(self):
                cleared.append(True)
            def replace_text(self, txt):
                replaced.append(txt)

        class _V24TextField(object):
            name = "label_input"
            display = True
            state = 0
            destroyed = False
            children = []
            def __init__(self):
                self.controller = _Controller()
            def insert_text(self, txt):
                replaced.append(txt)

        class BookmarkLocationWindow(object):
            name = "BookmarkLocationWindow"
            display = True
            state = 0
            destroyed = False
            def __init__(self, include_field=True):
                self.children = [_V24TextField()] if include_field else []
            def Confirm(self):
                confirmed.append(replaced[-1] if replaced else "default")
            def Close(self):
                closed.append(True)

        active_bw = [None]
        sess = types.SimpleNamespace(solarsystemid=30000142, shipid=90010001)
        cmd_svc = types.SimpleNamespace(CmdCreateBookmark=lambda: None)
        reg = types.SimpleNamespace(GetWindows=lambda: [active_bw[0]] if active_bw[0] else [])
        bm_svc = types.SimpleNamespace(
            BookmarkLocation=lambda loc_id, sub_id, name, note: fallback_calls.append((loc_id, name, note))
        )
        fake_uicore = types.SimpleNamespace(cmd=cmd_svc, registry=reg, layer=None, desktop=None)
        fake_sm = mock.MagicMock()
        fake_sm.GetService.side_effect = lambda n: {"bookmarkSvc": bm_svc}.get(n, mock.MagicMock())

        active_bw[0] = BookmarkLocationWindow(include_field=True)
        with mock.patch.dict(sys.modules, {"uthread": None}), \
             mock.patch.object(builtins, "session", sess, create=True), \
             mock.patch.object(hook, "__builtin__", types.SimpleNamespace(session=sess)), \
             mock.patch.object(hook.H, "_sleep_ms", create=True), \
             mock.patch.object(hook, "_install_flight_recorder_hooks"), \
             mock.patch.object(hook, "_install_warp_telemetry"), \
             mock.patch.object(hook, "_get_uicore", return_value=fake_uicore), \
             mock.patch.object(hook, "_wait_session_settled"), \
             mock.patch.object(hook, "_get_state_snapshot", return_value={"open_windows": [], "modals": [], "ui": {}, "space": {}, "skills_state": {}, "wallet_state": {}, "active_view": "inflight"}), \
             mock.patch.object(hook, "_settle_after_action", return_value=50):
            res1 = hook._execute_action(fake_sm, {"type": "save_location", "target": "Scenario Planet Spot"})
            self.assertTrue(res1.get("ok"), res1)
            self.assertTrue(cleared)
            self.assertEqual(confirmed, ["Scenario Planet Spot"])
            self.assertEqual(fallback_calls, [])

            # If BookmarkLocationWindow lacks any writable field, close modal instead of confirming default name.
            active_bw[0] = BookmarkLocationWindow(include_field=False)
            res2 = hook._execute_action(fake_sm, {"type": "save_location", "target": "Fallback Spot"})
            self.assertTrue(res2.get("ok"), res2)
            self.assertEqual(confirmed, ["Scenario Planet Spot"])
            self.assertEqual(closed, [True])
            self.assertEqual(fallback_calls, [(90010001, "Fallback Spot", "")])

    def test_resolve_secondary_view_surface_and_people_window_aliases(self):
        hook = _load_hook_with_py2_stubs()
        class _Dummy(object):
            destroyed = False
            display = True
            state = 0

        lbl = _Dummy()
        lbl.name = "offer_title"
        lbl.text = "30 Days Omega Clone State"
        lbl.children = []
        vgs_layer = _Dummy()
        vgs_layer.name = "l_virtual_goods_store"
        vgs_layer.children = [lbl]
        vs_layer = _Dummy()
        vs_layer.children = [vgs_layer]
        layers = _Dummy()
        layers.viewstate = vs_layer
        uic = _Dummy()
        uic.layer = layers

        sec_vi = _Dummy()
        sec_vi.name = "virtual_goods_store"
        vs = _Dummy()
        vs.activeView = None
        vs.secondaryView = None

        view_obj, layer = hook._resolve_secondary_view_surface(vs, sec_vi, uicore=uic)
        self.assertIsNone(view_obj)
        self.assertIs(layer, vgs_layer)
        inspected = hook._inspect_window_content(layer)
        self.assertGreaterEqual(inspected["populated_count"], 1)
        self.assertTrue(hook._target_matches_window("people", "addressbook", "Contacts", "AddressBookWindow"))


if __name__ == "__main__":
    unittest.main()
