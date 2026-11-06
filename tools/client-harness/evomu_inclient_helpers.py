# -*- coding: utf-8 -*-
"""Helper utilities for the EvoMu V24.01 in-client journey driver (Python 2.7)."""

import __builtin__
import json
import os
import socket
import time


def _sleep_ms(ms):
    sec = max(0.005, float(ms) / 1000.0)
    try:
        import blue
        blue.pyos.synchro.SleepWallclock(max(1, int(ms)))
        return
    except Exception:
        pass
    try:
        import uthread2
        uthread2.sleep(sec)
        return
    except Exception:
        pass
    time.sleep(sec)


def _trace(cfg, msg):
    try:
        out_dir = (cfg or {}).get("out_dir")
        if out_dir:
            with open(os.path.join(out_dir, "driver_trace.log"), "a") as tf:
                tf.write("[%.3f] %s\n" % (time.time(), msg))
                tf.flush()
    except Exception:
        pass


def _atomic_write_json(path, data):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=2, sort_keys=True)
        f.flush()
    if os.path.exists(path):
        try:
            os.remove(path)
        except Exception:
            pass
    os.rename(tmp, path)


class IpcClient(object):
    def __init__(self, port):
        self.sock = socket.create_connection(("127.0.0.1", int(port)), 15.0) if port else None
        if self.sock is not None:
            self.sock.settimeout(30.0)
        self.rfile = self.sock.makefile("r") if self.sock is not None else None

    def request(self, payload):
        if self.sock is None:
            return {"ok": False, "error": "no ipc socket"}
        self.sock.sendall(json.dumps(payload) + "\n")
        resp = self.rfile.readline()
        return json.loads(resp) if resp else {"ok": False, "error": "ipc eof"}

    def notify(self, payload):
        try:
            return self.request(payload)
        except Exception:
            return {"ok": False}

    def close(self):
        for h in (self.rfile, self.sock):
            try:
                if h is not None:
                    h.close()
            except Exception:
                pass


def _get_field(obj, name, default=None):
    if obj is None:
        return default
    if isinstance(obj, dict):
        return obj.get(name, default)
    if hasattr(obj, name):
        return getattr(obj, name)
    try:
        return obj[name]
    except Exception:
        return default


def _extract_id(res, keys=("itemID", "contractID", "fleetID", "jobID", "mailID", "eventID", "labelID", "listID", "corporationID", "allianceID", "structureID", "orderID")):
    if isinstance(res, (int, long)):
        return int(res)
    for k in keys:
        v = res.get(k) if isinstance(res, dict) else getattr(res, k, None)
        if v:
            return int(v)
    if isinstance(res, (tuple, list)) and len(res) > 0:
        first = res[0]
        if isinstance(first, (int, long)):
            return int(first)
        if hasattr(first, "GetMachoObjectBoundId"):
            return first
    return res


def _ensure_view(sm, target_view):
    uicore = getattr(__builtin__, "uicore", None)
    if uicore is not None and getattr(uicore, "layer", None) is not None:
        charsel = getattr(uicore.layer, "charsel", None)
        if charsel is not None and getattr(charsel, "isopen", False):
            try:
                charsel.CloseView()
            except Exception:
                pass
    vs = sm.GetService("viewState")
    for _ in range(100):
        cur = vs.GetCurrentViewInfo()
        cname = getattr(cur, "name", None) if cur is not None else None
        opening = getattr(vs, "isOpeningView", None)
        if not opening:
            if target_view == "hangar" and (
                vs.IsViewActive("hangar")
                or vs.IsViewActive("station")
                or cname in ("hangar", "station")
            ):
                break
            if vs.IsViewActive(target_view) or cname == target_view:
                break
        _sleep_ms(100)
    else:
        if not getattr(vs, "isOpeningView", None):
            cur = vs.GetCurrentViewInfo()
            cname = getattr(cur, "name", None) if cur is not None else None
            already = (
                target_view == "hangar" and (
                    vs.IsViewActive("hangar") or vs.IsViewActive("station") or cname in ("hangar", "station")
                )
            ) or vs.IsViewActive(target_view) or cname == target_view
            if not already:
                if hasattr(vs, "ChangePrimaryView"):
                    vs.ChangePrimaryView(target_view)
                else:
                    vs.ActivateView(target_view)
                _sleep_ms(300)
    loading = sm.GetService("loading")
    for _ in range(40):
        if not loading.IsLoading():
            break
        _sleep_ms(100)
    _wait_view_settled(sm)


def _wait_view_settled(sm, timeout_sec=10.0):
    vs = sm.GetService("viewState")
    loading = getattr(sm, "services", {}).get("loading")
    start = time.time()
    deadline = start + timeout_sec
    while time.time() < deadline:
        now = time.time()
        opening = getattr(vs, "isOpeningView", None)
        is_loading = bool(loading.IsLoading()) if loading is not None and hasattr(loading, "IsLoading") else False
        space_svc = getattr(sm, "services", {}).get("space")
        lazy_cnt = getattr(space_svc, "lazyLoadQueueCount", 0) if (space_svc is not None and now <= start + 1.2) else 0
        sess = getattr(__builtin__, "session", None)
        sess_busy = bool(sess is not None and (getattr(sess, "mutating", 0) or getattr(sess, "changing", None) is not None))
        sys_id = int(getattr(sess, "solarsystemid", 0) or 0) if sess is not None else 0
        in_space = bool(
            sess is not None and 30000000 <= sys_id < 34000000
            and not getattr(sess, "stationid", None) and not getattr(sess, "structureid", None)
        )
        bp_ok = True
        if in_space and not opening and vs.IsViewActive("inflight"):
            michelle = getattr(sm, "services", {}).get("michelle")
            bp = getattr(michelle, "_Michelle__bp", None) if michelle is not None else None
            if (bp is None or int(getattr(bp, "solarsystemID", 0) or 0) != sys_id) and now > start + 2.0 and michelle is not None:
                try:
                    michelle.AddBallpark(long(sys_id))
                    sm.RemoteSvc("beyonce").MachoBindObject(int(sys_id), None)
                    bp = getattr(michelle, "_Michelle__bp", None)
                except Exception:
                    pass
            bp_ok = bool(
                bp is not None and getattr(michelle, "bpReady", False)
                and int(getattr(bp, "solarsystemID", 0) or 0) == sys_id
                and getattr(bp, "validState", 0)
            )
        ip_svc = getattr(sm, "services", {}).get("infoPanel")
        ip_busy = False
        if ip_svc is not None and now <= start + 1.5:
            ip_busy = any(
                getattr(ip_svc, attr, None) is not None
                for attr in (
                    "operationInfoPanelUpdater",
                    "recommendationsInfoPanelUpdater",
                    "missionInfoPanelUpdater",
                    "airNpeInfoPanelUpdater",
                )
            )
        if not opening and not is_loading and not lazy_cnt and not sess_busy and not ip_busy and bp_ok:
            break
        _sleep_ms(50)
    _sleep_ms(100)


def _sync_quasar_gateway(sm, char_id=None):
    try:
        pgw = sm.GetService("publicGatewaySvc")
        sess = getattr(__builtin__, "session", None)
        uid_cur = int(getattr(sess, "userid", 1) or 1)
        gw = getattr(pgw, "publicGateway", None)
        if gw is not None:
            if hasattr(gw, "set_authenticated_user_id"):
                gw.set_authenticated_user_id(uid_cur)
            if char_id and hasattr(gw, "set_active_character_id"):
                gw.set_active_character_id(int(char_id))
            broker_details = gw.get_request_broker_details() if hasattr(gw, "get_request_broker_details") else {}
            if isinstance(broker_details, dict):
                ch_state = str(broker_details.get("channel_state", "")).upper()
            else:
                ch_state = str(getattr(broker_details, "channel_state", "")).upper()
            need_setup = (
                getattr(gw, "_evomu_dev_uid", None) != uid_cur
                or getattr(gw, "_evomu_dev_cid", None) != char_id
            )
            if not need_setup and ch_state not in ("READY", "CONNECTED", "CONNECTING"):
                need_setup = True
            if need_setup:
                tok = None
                try:
                    tok = sm.RemoteSvc("userSvc").GetUserToken()
                except Exception:
                    tok = None
                if tok and hasattr(pgw, "set_auth_token"):
                    pgw.set_auth_token(tok)
                    if ch_state not in ("READY", "CONNECTED", "CONNECTING") and hasattr(pgw, "connect"):
                        pgw.connect()
                elif tok and hasattr(gw, "set_auth_token"):
                    gw.set_auth_token(tok)
                    if ch_state not in ("READY", "CONNECTED", "CONNECTING") and hasattr(gw, "connect"):
                        gw.connect()
                elif hasattr(pgw, "setup_local_dev_token"):
                    pgw.setup_local_dev_token(uid_cur)
                if hasattr(gw, "get_request_broker_details"):
                    for _ in range(15):
                        bd = gw.get_request_broker_details()
                        st = str(bd.get("channel_state", "") if isinstance(bd, dict) else getattr(bd, "channel_state", "")).upper()
                        if st in ("READY", "CONNECTED"):
                            break
                        _sleep_ms(50)
                gw._evomu_dev_uid = uid_cur
                gw._evomu_dev_cid = char_id
        return pgw
    except Exception:
        return None


def _assert_ui_rendered(sm, expected_view):
    vs = sm.GetService("viewState")
    cur = vs.GetCurrentViewInfo()
    cname = getattr(cur, "name", None) if cur is not None else None
    if expected_view == "hangar":
        ok_view = bool(vs.IsViewActive("hangar") or vs.IsViewActive("station") or cname in ("hangar", "station"))
    else:
        ok_view = bool(vs.IsViewActive(expected_view) or cname == expected_view)
    if not ok_view:
        raise RuntimeError("UI view mismatch: expected %r, got current=%r" % (expected_view, cname))
    loading = sm.GetService("loading")
    if loading.IsLoading():
        raise RuntimeError("UI loading curtain still active (IsLoading() == True)")
    uicore = getattr(__builtin__, "uicore", None)
    if uicore is not None and getattr(uicore, "layer", None) is not None:
        llayer = getattr(uicore.layer, "loading", None)
        if llayer is not None and getattr(llayer, "display", False) and getattr(llayer, "state", 1) != 1:
            raise RuntimeError("UI loading layer still visible: state=%r" % (getattr(llayer, "state", None),))
    scene_mgr = sm.GetService("sceneManager")
    scene = scene_mgr.GetActiveScene()
    for _ in range(30):
        if scene is not None:
            break
        _sleep_ms(100)
        scene = scene_mgr.GetActiveScene()
    if scene is None:
        raise RuntimeError("sceneManager.GetActiveScene() is None for %r" % (expected_view,))
    godma = sm.GetService("godma")
    deadline_godma = time.time() + 5.0
    while getattr(godma, "state", None) == 2 and time.time() < deadline_godma:
        _sleep_ms(100)
    if not sm.IsServiceRunning("godma"):
        failed_svcs = [(k, getattr(v, "state", None)) for k, v in getattr(sm, "services", {}).items() if getattr(v, "state", None) != 4]
        raise RuntimeError("godma service not running: state=%r non_running=%r" % (getattr(godma, "state", None), failed_svcs))
    if expected_view == "inflight":
        michelle = sm.GetService("michelle")
        bp = getattr(michelle, "_Michelle__bp", None)
        for _ in range(30):
            if bp is not None:
                break
            _sleep_ms(100)
            bp = getattr(michelle, "_Michelle__bp", None)
        if bp is None:
            raise RuntimeError("michelle.GetBallpark() is None in inflight view")


def _wait_for_session(sm, user, password, target_char_id, timeout_sec=120.0):
    deadline = time.time() + timeout_sec
    tried_manual_login, tried_manual_select = False, False
    chosen_char_id, avail_ids = int(target_char_id or 0), []
    while time.time() < deadline:
        sess = getattr(__builtin__, "session", None)
        userid = getattr(sess, "userid", None) if sess is not None else None
        charid = getattr(sess, "charid", None) if sess is not None else None
        if userid:
            _sync_quasar_gateway(sm, int(charid) if charid else None)
        sess_busy = bool(
            sess is not None
            and (getattr(sess, "mutating", 0) or getattr(sess, "changing", None) is not None)
        )
        has_loc = bool(
            sess is not None
            and (
                getattr(sess, "stationid", None)
                or getattr(sess, "structureid", None)
                or getattr(sess, "solarsystemid", None)
            )
        )
        if charid and not sess_busy and has_loc and (
            not target_char_id
            or int(charid) == int(chosen_char_id)
            or (avail_ids and int(target_char_id) not in avail_ids and int(charid) in avail_ids)
        ):
            vs = sm.GetService("viewState")
            init_view = (
                "inflight"
                if (getattr(sess, "solarsystemid", None) and not getattr(sess, "stationid", None) and not getattr(sess, "structureid", None))
                else "hangar"
            )
            cur = vs.GetCurrentViewInfo()
            cname = getattr(cur, "name", None) if cur is not None else None
            if cname in (None, "login", "charsel") or getattr(vs, "isOpeningView", None):
                _sleep_ms(150)
                continue
            _sleep_ms(200)
            _sync_quasar_gateway(sm, int(charid))
            _ensure_view(sm, init_view)
            try:
                sm.GetService("shipTree")
            except Exception:
                pass
            return int(charid)
        conn_svc = sm.services.get("connection") if hasattr(sm, "services") else None
        macho_svc = sm.services.get("machoNet") if hasattr(sm, "services") else None
        conn_busy = bool(
            (
                conn_svc is not None
                and (
                    getattr(conn_svc, "connecting", False)
                    or getattr(conn_svc, "isConnecting", False)
                )
            )
            or (
                macho_svc is not None
                and (
                    getattr(macho_svc, "authenticating", 0)
                    or getattr(macho_svc, "gettingServerStatus", 0)
                    or getattr(macho_svc, "transportsByID", None)
                    or (hasattr(macho_svc, "IsConnected") and macho_svc.IsConnected())
                )
            )
        )
        if not userid and not tried_manual_login and not conn_busy and time.time() > deadline - 5.0:
            tried_manual_login = True
            try:
                if sm.IsServiceRunning("connection"):
                    sm.GetService("connection").Login(user, password, None, None)
                else:
                    sm.RemoteSvc("authentication").Login(user, password)
            except Exception:
                try:
                    sm.RemoteSvc("authentication").Login(user, password)
                except Exception:
                    pass
        if userid and not charid and not tried_manual_select:
            vs = sm.GetService("viewState")
            cur = vs.GetCurrentViewInfo()
            cname = getattr(cur, "name", None) if cur is not None else None
            view_ready = (cname == "charsel" and not getattr(vs, "isOpeningView", None))
            if view_ready or time.time() > deadline - (timeout_sec - 12.0):
                tried_manual_select = True
                chars = sm.RemoteSvc("charUnboundMgr").GetCharactersToSelect()
                avail_ids = [int(_get_field(c, "characterID", 0)) for c in (chars or []) if _get_field(c, "characterID", None)]
                if target_char_id and int(target_char_id) in avail_ids:
                    chosen_char_id = int(target_char_id)
                elif avail_ids:
                    chosen_char_id = avail_ids[0]
                if sess is not None:
                    sess.nextSessionChange = None
                sm.GetService("sessionMgr").PerformSessionChange("charsel", sm.RemoteSvc("charUnboundMgr").SelectCharacterID, int(chosen_char_id), None, True)
        _sleep_ms(250)
    sess = getattr(__builtin__, "session", None)
    if getattr(sess, "charid", None):
        return int(sess.charid)
    raise RuntimeError("Timed out waiting for V24.01 session character selection")


def _live_session_attr(attr, default=None):
    sess = getattr(__builtin__, "session", None)
    val = getattr(sess, attr, default) if sess is not None else default
    return default if val is None else val


def _wait_session_attr(attr, expected, timeout_sec=5.0):
    deadline = time.time() + float(timeout_sec)
    while time.time() < deadline:
        sess = getattr(__builtin__, "session", None)
        cur = getattr(sess, attr, None) if sess is not None else None
        busy = bool(
            sess is not None
            and (getattr(sess, "mutating", 0) or getattr(sess, "changing", None) is not None)
        )
        if cur == expected and not busy:
            _sleep_ms(200)
            return cur
        _sleep_ms(50)
    return _live_session_attr(attr, expected)


def _try_slash(sm, cmd):
    try:
        res = sm.RemoteSvc("slash").SlashCmd(cmd)
        if cmd.startswith("/giveskill ") or cmd.startswith("/setskill "):
            _sleep_ms(150)
        return res
    except Exception:
        return None


def _get_beyonce(sm, sys_id):
    try:
        michelle = sm.GetService("michelle")
        rp = michelle.GetRemotePark()
        bp = getattr(michelle, "_Michelle__bp", None)
        if (
            rp is not None
            and getattr(rp, "boundObject", None) is not None
            and bp is not None
            and int(getattr(bp, "solarsystemID", 0) or 0) == int(sys_id)
        ):
            return rp
    except Exception:
        pass
    b_obj, _ = sm.RemoteSvc("beyonce").MachoBindObject(int(sys_id), None)
    return b_obj


def _sleep_sim_ms(ms):
    try:
        import blue
        blue.synchro.SleepSim(max(1, int(ms)))
        return
    except Exception:
        pass
    try:
        import blue
        blue.pyos.synchro.SleepSim(max(1, int(ms)))
        return
    except Exception:
        pass
    _sleep_ms(ms)


_WARP_SIGNAL_STATE = {"hooked": False, "in_warp": False, "started": 0, "finished": 0}


def _on_warp_signal_started(*_args, **_kwargs):
    _WARP_SIGNAL_STATE["in_warp"] = True
    _WARP_SIGNAL_STATE["started"] += 1


def _on_warp_signal_finished(*_args, **_kwargs):
    _WARP_SIGNAL_STATE["in_warp"] = False
    _WARP_SIGNAL_STATE["finished"] += 1


def _ensure_warp_signal_tracker(sm):
    if _WARP_SIGNAL_STATE["hooked"]:
        return
    try:
        from eve.client.script.environment.effects import warp_signals

        sig_start = getattr(warp_signals, "on_warp_activated", None) or getattr(
            warp_signals, "on_warp_started", None
        )
        if sig_start is not None:
            sig_start.connect(_on_warp_signal_started)
        warp_signals.on_warp_finished.connect(_on_warp_signal_finished)
        _WARP_SIGNAL_STATE["hooked"] = True
        return
    except Exception:
        pass
    try:
        space_svc = sm.GetService("space")
        for attr in ("warp_signals", "warp_signal", "warpSignals"):
            ws = getattr(space_svc, attr, None)
            if ws is not None and hasattr(ws, "on_warp_finished"):
                if hasattr(ws, "on_warp_activated"):
                    ws.on_warp_activated.connect(_on_warp_signal_started)
                elif hasattr(ws, "on_warp_started"):
                    ws.on_warp_started.connect(_on_warp_signal_started)
                ws.on_warp_finished.connect(_on_warp_signal_finished)
                _WARP_SIGNAL_STATE["hooked"] = True
                break
    except Exception:
        pass


def _wait_warp_fx_settled(sm, timeout_sec=4.0, min_finished=0):
    _ensure_warp_signal_tracker(sm)
    try:
        sess = getattr(__builtin__, "session", None)
        sid = int(getattr(sess, "shipid", 0) or 0) if sess is not None else 0
        michelle = sm.services.get("michelle") if hasattr(sm, "services") else None
        space_svc = sm.services.get("space") if hasattr(sm, "services") else None
        fx_svc = sm.services.get("fxSequencer") if hasattr(sm, "services") else None
        deadline = time.time() + float(timeout_sec)
        while time.time() < deadline:
            sess = getattr(__builtin__, "session", None)
            if getattr(sess, "stationid", None) is not None or getattr(sess, "structureid", None) is not None:
                _WARP_SIGNAL_STATE["in_warp"] = False
                break
            bp = michelle.GetBallpark() if (sid and michelle is not None) else None
            ball = bp.GetBall(sid) if (bp is not None and sid) else None
            warping_ball = ball is not None and int(getattr(ball, "mode", 0) or 0) == 3
            warping_fx = (
                space_svc is not None
                and getattr(space_svc, "warpDestinationCache", None) is not None
            )
            if not warping_fx and fx_svc is not None and sid:
                try:
                    if hasattr(fx_svc, "GetAllBallActivationNames") and "effects.Warping" in (
                        fx_svc.GetAllBallActivationNames(sid) or ()
                    ):
                        warping_fx = True
                    if not warping_fx and hasattr(fx_svc, "balls"):
                        for eff in (fx_svc.balls.get(sid) or {}).values():
                            sball = getattr(eff, "shipBall", None)
                            if sball is not None and int(getattr(sball, "mode", 0) or 0) == 3:
                                warping_fx = True
                                break
                except Exception:
                    pass
            signals_settled = not _WARP_SIGNAL_STATE["hooked"] or (
                not _WARP_SIGNAL_STATE["in_warp"]
                and _WARP_SIGNAL_STATE["finished"] >= int(min_finished)
            )
            if not warping_ball and not warping_fx and signals_settled:
                break
            _sleep_sim_ms(100)
        sess = getattr(__builtin__, "session", None)
        if getattr(sess, "stationid", None) is None and getattr(sess, "structureid", None) is None:
            try:
                sm.RemoteSvc("timeControl").AdvanceClock(2)
            except Exception:
                pass
            _sleep_sim_ms(1500)
            _sleep_ms(300)
        if not _WARP_SIGNAL_STATE["hooked"]:
            _sleep_sim_ms(600)
        elif _WARP_SIGNAL_STATE["in_warp"]:
            _WARP_SIGNAL_STATE["in_warp"] = False
    except Exception:
        pass
    _sleep_ms(150)


def _warp_full(sm, b_obj, tid, stuff_type="item", distance=0.0):
    _ensure_warp_signal_tracker(sm)
    for _ in range(4):
        sm.RemoteSvc("slash").SlashCmd("/heal")
        try:
            b_obj.CmdWarpToStuff(stuff_type, tid, float(distance))
        except Exception as exc:
            msg = str(exc)
            if "WarpDistanceTooShort" in msg:
                break
            if "ShipInWarp" in msg or "ShipAlreadyInWarp" in msg:
                sm.RemoteSvc("timeControl").AdvanceClock(120)
                _sleep_sim_ms(400)
                _wait_warp_fx_settled(sm, timeout_sec=4.0)
                continue
            raise
        _sleep_ms(30)
        sm.RemoteSvc("timeControl").AdvanceClock(240)
        _sleep_sim_ms(400)
        _wait_warp_fx_settled(sm, timeout_sec=4.0)
        break
    _wait_view_settled(sm)


def _undock_ship(sm):
    _wait_view_settled(sm)
    sess = getattr(__builtin__, "session", None)
    if getattr(sess, "solarsystemid", None) and not getattr(sess, "stationid", None):
        _ensure_view(sm, "inflight")
        _ensure_warp_signal_tracker(sm)
        return
    try:
        sm.RemoteSvc("timeControl").ClearSessionTimer()
    except Exception:
        pass
    _sleep_ms(600)
    sm.RemoteSvc("ship").Undock()
    deadline = time.time() + 10.0
    while time.time() < deadline:
        sess = getattr(__builtin__, "session", None)
        if (
            getattr(sess, "solarsystemid", None)
            and not getattr(sess, "stationid", None)
            and not getattr(sess, "mutating", 0)
            and getattr(sess, "changing", None) is None
        ):
            break
        _sleep_ms(100)
    try:
        sm.RemoteSvc("timeControl").ClearSessionTimer()
    except Exception:
        pass
    _ensure_view(sm, "inflight")
    _ensure_warp_signal_tracker(sm)


def _dock_station(sm, station_id=60003760):
    sess = getattr(__builtin__, "session", None)
    if getattr(sess, "solarsystemid", None) and not getattr(sess, "stationid", None):
        _sleep_sim_ms(1200)
        _sleep_ms(1200)
    _wait_warp_fx_settled(sm, timeout_sec=35.0)
    _wait_view_settled(sm)
    try:
        sm.RemoteSvc("timeControl").ClearSessionTimer()
    except Exception:
        pass
    sm.RemoteSvc("stationSvc").Dock(station_id)
    deadline = time.time() + 10.0
    while time.time() < deadline:
        sess = getattr(__builtin__, "session", None)
        if (
            getattr(sess, "stationid", None) == station_id
            and not getattr(sess, "mutating", 0)
            and getattr(sess, "changing", None) is None
        ):
            break
        _sleep_ms(100)
    try:
        sm.RemoteSvc("timeControl").ClearSessionTimer()
    except Exception:
        pass
    _ensure_view(sm, "hangar")


def _ensure_docked_jita(sm, ipc):
    _wait_view_settled(sm)
    sess = getattr(__builtin__, "session", None)
    station_id = getattr(sess, "stationid", None)
    if station_id == 60003760:
        _ensure_view(sm, "hangar")
        return
    if getattr(sess, "solarsystemid", None) and not station_id:
        _sleep_sim_ms(1200)
    _wait_warp_fx_settled(sm, timeout_sec=35.0)
    try:
        beyonce, _ = sm.RemoteSvc("beyonce").MachoBindObject(30000142, None)
        beyonce.CmdStop()
    except Exception:
        pass
    ipc.request({"cmd": "advance_clock", "secs": 12})
    _sleep_sim_ms(1200)
    _sleep_ms(1200)
    _wait_warp_fx_settled(sm, timeout_sec=5.0)
    try:
        sm.RemoteSvc("stationSvc").Dock(60003760)
    except Exception:
        _try_slash(sm, "/tr 30000142")
        _wait_session_attr("solarsystemid2", 30000142, timeout_sec=3.0)
        _wait_view_settled(sm)
        ipc.request({"cmd": "advance_clock", "secs": 12})
        sm.RemoteSvc("stationSvc").Dock(60003760)
    deadline = time.time() + 10.0
    while time.time() < deadline:
        sess = getattr(__builtin__, "session", None)
        if getattr(sess, "stationid", None) == 60003760 and getattr(sess, "changing", None) is None:
            break
        _sleep_ms(100)
    try:
        sm.RemoteSvc("timeControl").ClearSessionTimer()
    except Exception:
        pass
    _ensure_view(sm, "hangar")
