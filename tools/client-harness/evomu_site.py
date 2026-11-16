# -*- coding: utf-8 -*-
"""Early Python 2.7 startup hook for real V24.01 `exefile` inside EvoMu harness.

Loaded via `sitecustomize.py` before `autoexec` starts. Hooks `logmodule`
exception reporting (recording every exception/traceback without suppressing it),
disables XMPP TLS on localhost (`chatserver_disable_tls`), and schedules
`evomu_inclient_driver.schedule_startup()` on the main Stackless thread as soon
as `__builtin__.sm` and `machoNet` are running.
"""

import __builtin__
import math
import os
import sys
import time
import traceback

sys.dont_write_bytecode = True

_HOOKED_LOGMODULE = False
_HOOKED_TARGETS = set()
_HOOKED_XMPP_PREFS = False
_HOOKED_XMPP_PORT = False
_HOOKED_FLATBUFFER_CDN = False
_HOOKED_TYPELIST_PRELOAD = False
_HOOKED_CAMERA_GUARD_TARGETS = set()
_DRIVER_SCHEDULED = False
_META_HOOK_INSTALLED = False
_RECENT_EXCEPTIONS = []
_EXC_SEQ = 0
_CAMERA_MAX_SAFE_COORD = 1e16


def _log_exc_line(msg):
    global _EXC_SEQ
    _EXC_SEQ += 1
    try:
        lines = [ln.strip() for ln in str(msg).splitlines() if ln.strip()]
        headline = lines[-1] if lines else "Client Python exception"
        for ln in reversed(lines):
            if "Error:" in ln or "Exception:" in ln:
                headline = ln
                break
        _RECENT_EXCEPTIONS.append({
            "seq": _EXC_SEQ,
            "ts": round(time.time(), 3),
            "headline": headline[:200],
            "trace": str(msg)[:1200],
        })
        if len(_RECENT_EXCEPTIONS) > 40:
            del _RECENT_EXCEPTIONS[:-30]
    except Exception:
        pass
    cfg_path = os.environ.get("EVOMU_INCLIENT_CFG", "")
    if cfg_path:
        try:
            out_dir = os.path.dirname(cfg_path)
            with open(os.path.join(out_dir, "client_tracebacks.log"), "a") as tf:
                tf.write("%s\n" % (msg,))
        except Exception:
            pass
    try:
        sys.stderr.write("[evomu_site] %s\n" % (msg,))
        sys.stderr.flush()
    except Exception:
        pass


_ORIG_LOG_FUNCS = []


def _propagate_log_wrappers(replacements):
    for mod in list(sys.modules.values()):
        if mod is None:
            continue
        try:
            d = getattr(mod, "__dict__", None)
            if not isinstance(d, dict):
                continue
            for k, v in list(d.items()):
                for orig_fn, wrap_fn in replacements:
                    if v is orig_fn:
                        d[k] = wrap_fn
        except Exception:
            pass


def _wrap_log_target(target, label):
    if target is None:
        return
    for attr in ("LogException", "_LogException", "LogTraceback"):
        orig_fn = getattr(target, attr, None)
        if orig_fn is None or getattr(orig_fn, "_evomu_wrapped", False):
            continue
        def _make_wrapper(orig, attr_name):
            def _wrapped(*args, **kwargs):
                if attr_name == "LogTraceback" and kwargs.get("severity") in (1, 2) and sys.exc_info()[0] is None:
                    return orig(*args, **kwargs)
                tb = traceback.format_exc()
                inner_tb = ""
                try:
                    if args and isinstance(args[0], tuple) and len(args[0]) >= 3 and args[0][2] is not None:
                        inner_tb = "\nINNER TRACEBACK:\n" + "".join(traceback.format_tb(args[0][2]))
                except Exception:
                    pass
                stack = "".join(traceback.format_stack()[:-1])
                _log_exc_line("%s.%s args=%r kwargs=%r%s\n%s\nCALLSTACK:\n%s" % (label, attr_name, args, kwargs, inner_tb, tb, stack))
                return orig(*args, **kwargs)
            _wrapped._evomu_wrapped = True
            return _wrapped
        wrap_fn = _make_wrapper(orig_fn, attr)
        setattr(target, attr, wrap_fn)
        _ORIG_LOG_FUNCS.append((orig_fn, wrap_fn))


def _install_logmodule_hook(propagate=False):
    global _HOOKED_LOGMODULE
    if _HOOKED_LOGMODULE and not propagate:
        return True
    lm = sys.modules.get("logmodule")
    log_mod = sys.modules.get("log")
    svc_mod = sys.modules.get("service")
    blue_mod = sys.modules.get("blue")
    slu = sys.modules.get("stdlogutils")
    if lm is None and log_mod is None:
        return False
    if lm is not None and "logmodule" not in _HOOKED_TARGETS:
        _wrap_log_target(lm, "logmodule")
        _HOOKED_TARGETS.add("logmodule")
    if log_mod is not None and "log" not in _HOOKED_TARGETS:
        _wrap_log_target(log_mod, "log")
        _HOOKED_TARGETS.add("log")
    if svc_mod is not None and hasattr(svc_mod, "CoreMixIn") and "service" not in _HOOKED_TARGETS:
        _wrap_log_target(svc_mod.CoreMixIn, "service.CoreMixIn")
        _HOOKED_TARGETS.add("service")
    if blue_mod is not None and hasattr(blue_mod, "pyos") and "blue" not in _HOOKED_TARGETS:
        orig_eh = getattr(blue_mod.pyos, "exceptionHandler", None)
        if orig_eh is not None and not getattr(orig_eh, "_evomu_wrapped", False):
            def _wrapped_eh(etype, eval_, etb):
                try:
                    _log_exc_line(
                        "blue.pyos.exceptionHandler: %s"
                        % ("".join(traceback.format_exception(etype, eval_, etb)),)
                    )
                except Exception:
                    pass
                return orig_eh(etype, eval_, etb)
            _wrapped_eh._evomu_wrapped = True
            blue_mod.pyos.exceptionHandler = _wrapped_eh
        _HOOKED_TARGETS.add("blue")

    orig_excepthook = getattr(sys, "excepthook", None)
    if orig_excepthook is not None and not getattr(orig_excepthook, "_evomu_hooked", False):
        def _wrapped_excepthook(etype, eval_, etb):
            try:
                _log_exc_line(
                    "sys.excepthook: %s"
                    % ("".join(traceback.format_exception(etype, eval_, etb)),)
                )
            except Exception:
                pass
            return orig_excepthook(etype, eval_, etb)
        _wrapped_excepthook._evomu_hooked = True
        sys.excepthook = _wrapped_excepthook

    if slu is not None and hasattr(slu, "EveExceptionsFormatter") and "stdlogutils" not in _HOOKED_TARGETS:
        fmt_cls = slu.EveExceptionsFormatter
        orig_fmt = getattr(fmt_cls, "format", None)
        if orig_fmt is not None and not getattr(orig_fmt, "_evomu_wrapped", False):
            def _wrapped_fmt(self, record):
                res = orig_fmt(self, record)
                try:
                    if getattr(record, "exc_info", None) and int(getattr(record, "levelno", 40) or 40) >= 40:
                        _log_exc_line(
                            "stdlogutils.EveExceptionsFormatter logger=%s:\n%s"
                            % (getattr(record, "name", "?"), res)
                        )
                except Exception:
                    pass
                return res
            _wrapped_fmt._evomu_wrapped = True
            fmt_cls.format = _wrapped_fmt
        _HOOKED_TARGETS.add("stdlogutils")

    if propagate:
        _propagate_log_wrappers(_ORIG_LOG_FUNCS)

    if len(_HOOKED_TARGETS) >= 5:
        _HOOKED_LOGMODULE = True
    return True


def _install_xmpp_plaintext_guard():
    global _HOOKED_XMPP_PREFS
    if _HOOKED_XMPP_PREFS:
        return
    ep = sys.modules.get("eveprefs")
    p = getattr(ep, "prefs", None) if ep is not None else getattr(__builtin__, "prefs", None)
    if p is not None and hasattr(p, "SetValue"):
        try:
            p.SetValue("chatserver_disable_tls", 1)
            _HOOKED_XMPP_PREFS = True
        except Exception:
            pass


def _resolve_xmpp_port():
    """Return configured non-default XMPP TCP port (or None to keep default 5222)."""
    cfg_path = os.environ.get("EVOMU_INCLIENT_CFG", "")
    if cfg_path and os.path.exists(cfg_path):
        try:
            import json
            with open(cfg_path, "r") as f:
                p = int(json.load(f).get("xmpp_port", 0) or 0)
            if 1 <= p <= 65535:
                return p
        except Exception:
            pass
    for env_key in ("EVOMU_XMPP_PORT", "EVEMU_XMPP_PORT"):
        raw = os.environ.get(env_key, "").strip()
        if raw.isdigit():
            p = int(raw)
            if 1 <= p <= 65535:
                return p
    try:
        sm = getattr(__builtin__, "sm", None)
        if sm is not None:
            mn = sm.GetService("machoNet")
            gcfg = mn.GetGlobalConfig() if mn is not None and hasattr(mn, "GetGlobalConfig") else {}
            if hasattr(gcfg, "get"):
                val = gcfg.get("ejabberd_client_port") or gcfg.get("ejabberdPort")
                if val is not None:
                    p = int(val)
                    if 1 <= p <= 65535:
                        return p
    except Exception:
        pass
    return None


def _install_xmpp_port_guard():
    """Monkey-patch `xmpp.xmppConnection.XmppConnection.__init__` so `self.port`
    uses `_resolve_xmpp_port()` instead of the hardcoded `self.port = 5222` in V24.01."""
    global _HOOKED_XMPP_PORT
    if _HOOKED_XMPP_PORT:
        return
    xc_mod = sys.modules.get("xmpp.xmppConnection")
    if xc_mod is None:
        return
    xc_cls = getattr(xc_mod, "XmppConnection", None)
    if xc_cls is None:
        return
    orig_init = getattr(xc_cls, "__init__", None)
    if orig_init is None or getattr(orig_init, "_evomu_xmpp_port_patched", False):
        _HOOKED_XMPP_PORT = True
        return

    def _patched_init(self, *args, **kwargs):
        orig_init(self, *args, **kwargs)
        port = _resolve_xmpp_port()
        if port is not None:
            self.port = int(port)

    _patched_init._evomu_xmpp_port_patched = True
    try:
        xc_cls.__init__ = _patched_init
        _HOOKED_XMPP_PORT = True
    except Exception:
        pass


class _UnsignedCrc32ZlibProxy(object):
    """Wrap `zlib` so `crc32` returns an unsigned 32-bit int on Python 2.7 (`#1128`)."""

    _evomu_unsigned_crc32 = True

    def __init__(self, orig_zlib):
        self._orig = orig_zlib

    def crc32(self, data, *args):
        return self._orig.crc32(data, *args) & 0xFFFFFFFF

    def __getattr__(self, name):
        return getattr(self._orig, name)


def _load_httplib():
    try:
        import httplib
        return httplib
    except ImportError:
        import http.client as httplib
        return httplib


def _strip_host_port(netloc):
    if netloc.startswith("["):
        end = netloc.find("]")
        if end != -1:
            host = netloc[1:end]
            rest = netloc[end + 1:]
            return host, rest.startswith(":") and len(rest) > 1
    if ":" in netloc:
        host, port_part = netloc.rsplit(":", 1)
        if port_part.isdigit():
            return host, True
    return netloc, False


def _install_flatbuffer_cdn_http_guard():
    """Patch `content.client.flatbuffer.provider` so `http://host:port/...` CDN URLs use
    plain HTTP preserving `:port` instead of `HTTPSConnection(..., port=443)` and so
    `zlib.crc32` returns unsigned uint32 values matching server protobuf checksums (`#1128`)."""
    global _HOOKED_FLATBUFFER_CDN
    if _HOOKED_FLATBUFFER_CDN:
        return
    provider_mod = sys.modules.get("content.client.flatbuffer.provider")
    if provider_mod is None:
        return
    provider_cls = getattr(provider_mod, "FlatbufferContentProvider", None)
    if provider_cls is None:
        return

    cur_zlib = getattr(provider_mod, "zlib", None)
    if cur_zlib is not None and not getattr(cur_zlib, "_evomu_unsigned_crc32", False):
        try:
            provider_mod.zlib = _UnsignedCrc32ZlibProxy(cur_zlib)
        except Exception:
            pass

    orig_retrieve = getattr(provider_cls, "retrieve_data_from_cdn", None)
    orig_send_http = getattr(provider_cls, "_send_http_request", None)
    separate_host_and_request = getattr(provider_mod, "separate_host_and_request", None)
    if orig_retrieve is None or orig_send_http is None:
        return
    if getattr(orig_send_http, "_evomu_flatbuffer_cdn_patched", False):
        _HOOKED_FLATBUFFER_CDN = True
        return

    active_schemes = {}

    def _patched_retrieve(self, url, crc, *args, **kwargs):
        scheme = None
        key = None
        if url is not None and hasattr(url, "split") and "://" in url:
            scheme = url.split("://", 1)[0].lower()
            if separate_host_and_request is not None:
                try:
                    host, req_path = separate_host_and_request(url)
                    key = (id(self), host, req_path)
                except Exception:
                    key = None
        if key is not None and scheme is not None:
            active_schemes.setdefault(key, []).append(scheme)
        try:
            return orig_retrieve(self, url, crc, *args, **kwargs)
        finally:
            if key is not None and key in active_schemes:
                stack = active_schemes[key]
                if stack:
                    stack.pop()
                if not stack:
                    del active_schemes[key]

    def _patched_send_http_request(self, host, request, timeout_seconds=10):
        stack = active_schemes.get((id(self), host, request))
        scheme = stack[-1] if stack else None
        netloc = host
        req_path = request

        if netloc and "://" in netloc:
            explicit_scheme, rest = netloc.split("://", 1)
            scheme = explicit_scheme.lower()
            if "/" in rest:
                netloc, suffix = rest.split("/", 1)
                if not req_path:
                    req_path = "/" + suffix
            else:
                netloc = rest

        hostname, has_explicit_port = _strip_host_port(netloc or "")
        is_loopback = hostname.lower() in ("127.0.0.1", "localhost", "::1")
        use_plain_http = (scheme == "http") or (scheme is None and is_loopback)

        if use_plain_http or (scheme == "https" and has_explicit_port):
            httplib = _load_httplib()
            conn_cls = httplib.HTTPConnection if use_plain_http else httplib.HTTPSConnection
            conn = conn_cls(netloc, timeout=timeout_seconds)
            try:
                conn.putrequest("GET", req_path or "/")
                conn.endheaders()
                response = conn.getresponse()
                data = response.read()
                if response.status < 200 or response.status >= 300:
                    raise httplib.HTTPException(
                        "CDN HTTP %s %s for %s"
                        % (response.status, getattr(response, "reason", ""), req_path)
                    )
                return data
            finally:
                try:
                    conn.close()
                except Exception:
                    pass

        return orig_send_http(self, host, request, timeout_seconds)

    _patched_retrieve._evomu_flatbuffer_cdn_patched = True
    _patched_send_http_request._evomu_flatbuffer_cdn_patched = True
    try:
        provider_cls.retrieve_data_from_cdn = _patched_retrieve
        provider_cls._send_http_request = _patched_send_http_request
        _HOOKED_FLATBUFFER_CDN = True
    except Exception:
        pass


def _install_fsd_typelist_preload_hook():
    """Ensure BuiltDataLoader.GetData never returns None when asynchronous loading
    has not finished yet, and eagerly pre-load evetypes.storages.TypeListLoader (#796, #797)."""
    global _HOOKED_TYPELIST_PRELOAD
    if _HOOKED_TYPELIST_PRELOAD:
        return
    bdl_mod = sys.modules.get("fsdBuiltData.common.base")
    if bdl_mod is not None:
        bdl_cls = getattr(bdl_mod, "BuiltDataLoader", None)
        if bdl_cls is not None and not getattr(bdl_cls, "_evomu_getdata_patched", False):
            orig_get_data = getattr(bdl_cls, "GetData", None)
            if callable(orig_get_data):
                def _evomu_safe_get_data(cls):
                    data = orig_get_data.im_func(cls) if hasattr(orig_get_data, "im_func") else orig_get_data()
                    if data is None:
                        load_bin = getattr(cls, "_BuiltDataLoader__LoadBinary", None)
                        if callable(load_bin):
                            try:
                                load_bin(False)
                            except Exception:
                                pass
                            data = getattr(cls, "_BuiltDataLoader__data", None)
                    return data
                try:
                    bdl_cls.GetData = classmethod(_evomu_safe_get_data)
                    bdl_cls._evomu_getdata_patched = True
                except Exception:
                    pass
    et_storages = sys.modules.get("evetypes.storages")
    if et_storages is not None:
        tll = getattr(et_storages, "TypeListLoader", None)
        if tll is not None:
            try:
                if getattr(tll, "_BuiltDataLoader__data", None) is None:
                    load_bin = getattr(tll, "_BuiltDataLoader__LoadBinary", None)
                    if callable(load_bin):
                        load_bin(False)
                _HOOKED_TYPELIST_PRELOAD = True
            except Exception:
                pass


def _is_finite_safe_vec3(vec, limit=_CAMERA_MAX_SAFE_COORD):
    if vec is None:
        return False
    try:
        if len(vec) != 3:
            return False
        for c in vec:
            val = float(c)
            if math.isnan(val) or math.isinf(val) or abs(val) > limit:
                return False
        return True
    except Exception:
        return False


def _vec3_tuple(vec, fallback):
    try:
        if vec is not None and len(vec) == 3:
            vals = (float(vec[0]), float(vec[1]), float(vec[2]))
            if not any(math.isnan(c) or math.isinf(c) for c in vals):
                return vals
    except Exception:
        pass
    return (float(fallback[0]), float(fallback[1]), float(fallback[2]))


def _safe_direction_vec3(from_pos, to_pos, fallback=(0.0, 0.0, 1.0)):
    fx, fy, fz = _vec3_tuple(from_pos, (0.0, 0.0, 0.0))
    tx, ty, tz = _vec3_tuple(to_pos, fallback)
    dx = tx - fx
    dy = ty - fy
    dz = tz - fz
    scale = max(abs(dx), abs(dy), abs(dz))
    if scale <= 0.0 or math.isnan(scale) or math.isinf(scale):
        return _safe_direction_vec3((0.0, 0.0, 0.0), fallback, (0.0, 0.0, 1.0)) if fallback != (0.0, 0.0, 1.0) else (0.0, 0.0, 1.0)
    sx = dx / scale
    sy = dy / scale
    sz = dz / scale
    length = math.sqrt(sx * sx + sy * sy + sz * sz)
    if length <= 0.0 or math.isnan(length) or math.isinf(length):
        return (0.0, 0.0, 1.0)
    return (sx / length, sy / length, sz / length)


def _safe_tactical_new_direction(direction):
    if direction is None or len(direction) != 3:
        return (0.0, 0.7071067811865475, 0.7071067811865475)
    try:
        dx = float(direction[0])
        dz = float(direction[2])
    except Exception:
        return (0.0, 0.7071067811865475, 0.7071067811865475)
    if any(math.isnan(c) or math.isinf(c) for c in (dx, dz)):
        return (0.0, 0.7071067811865475, 0.7071067811865475)
    scale2 = max(abs(dx), abs(dz))
    if scale2 <= 0.0:
        return (0.0, 0.7071067811865475, 0.7071067811865475)
    y = scale2 * math.sqrt((dx / scale2) ** 2 + (dz / scale2) ** 2)
    return _safe_direction_vec3((0.0, 0.0, 0.0), (dx, y, dz), (0.0, 0.7071067811865475, 0.7071067811865475))


def _sanitize_camera_state(cam):
    changed = False
    default_at = _vec3_tuple(getattr(cam, "default_atPosition", (0.0, 0.0, 0.0)), (0.0, 0.0, 0.0))
    default_eye = _vec3_tuple(getattr(cam, "default_eyePosition", (0.0, 500.0, 1000.0)), (0.0, 500.0, 1000.0))
    at_pos = getattr(cam, "_atPosition", None)
    eye_pos = getattr(cam, "_eyePosition", None)
    if not _is_finite_safe_vec3(at_pos):
        cam._atPosition = list(default_at)
        at_pos = cam._atPosition
        changed = True
    if not _is_finite_safe_vec3(eye_pos):
        new_eye = list(default_eye)
        if _vec3_tuple(new_eye, default_eye) == _vec3_tuple(at_pos, default_at):
            new_eye = [at_pos[0], at_pos[1] + 500.0, at_pos[2] + 1000.0]
        cam._eyePosition = new_eye
        eye_pos = cam._eyePosition
        changed = True
    if _vec3_tuple(eye_pos, default_eye) == _vec3_tuple(at_pos, default_at):
        cam._eyePosition = [float(at_pos[0]), float(at_pos[1]) + 500.0, float(at_pos[2]) + 1000.0]
        changed = True
    for attr in ("_eyeOffset", "_atOffset", "_eyeTransitOffset", "_atTransitOffset"):
        val = getattr(cam, attr, None)
        if val is not None and not _is_finite_safe_vec3(val):
            setattr(cam, attr, None)
            changed = True
    return changed


def _michelle_ballpark_missing(michelle):
    if michelle is None:
        return True
    if hasattr(michelle, "_Michelle__bp") and getattr(michelle, "_Michelle__bp", None) is None:
        return True
    if hasattr(michelle, "GetBallpark"):
        try:
            bp = michelle.GetBallpark(False)
        except TypeError:
            try:
                bp = michelle.GetBallpark()
            except Exception:
                bp = None
        except Exception:
            bp = None
        if bp is None:
            return True
    return False


def _is_docked_or_ballpark_missing(svc_or_self=None, mod=None):
    sess = getattr(__builtin__, "session", None)
    if sess is not None and (
        getattr(sess, "stationid", None) is not None
        or getattr(sess, "structureid", None) is not None
    ):
        return True
    if svc_or_self is not None and hasattr(svc_or_self, "GetBallpark"):
        try:
            if svc_or_self.GetBallpark() is None:
                return True
        except Exception:
            return True
    get_michelle = getattr(mod, "GetMichelle", None) if mod is not None else None
    if callable(get_michelle):
        try:
            if _michelle_ballpark_missing(get_michelle()):
                return True
        except Exception:
            return True
    sm = getattr(__builtin__, "sm", None)
    if sm is not None:
        michelle = None
        services = getattr(sm, "services", None)
        if isinstance(services, dict):
            michelle = services.get("michelle")
        if michelle is None and hasattr(sm, "GetService"):
            try:
                if not hasattr(sm, "IsServiceRunning") or sm.IsServiceRunning("michelle"):
                    michelle = sm.GetService("michelle")
            except Exception:
                michelle = None
        if michelle is not None and _michelle_ballpark_missing(michelle):
            return True
    return False


def _make_arrival_method_guard(orig_fn, mod=None, clear_tracked=False):
    def _wrapped(self, *args, **kwargs):
        if _is_docked_or_ballpark_missing(self, mod):
            if clear_tracked and hasattr(self, "tracked_slim_item"):
                self.tracked_slim_item = None
            return None
        try:
            return orig_fn(self, *args, **kwargs)
        except TypeError as exc:
            if _is_docked_or_ballpark_missing(self, mod) or "NoneType" in str(exc):
                if clear_tracked and hasattr(self, "tracked_slim_item"):
                    self.tracked_slim_item = None
                return None
            raise

    _wrapped._evomu_camera_guard = True
    return _wrapped


def _patch_location_arrival_cls(cls, mod=None):
    if cls is None:
        return False
    patched = False
    for method_name, clear_tracked in (
        ("on_warp_finished", True),
        ("check_indicate_arrival", True),
        ("_indicate_arrival", True),
        ("get_closest_location_slim_item", False),
        ("get_nearest_celestial_to_player", False),
    ):
        orig_attr = getattr(cls, method_name, None)
        if orig_attr is None or getattr(orig_attr, "_evomu_camera_guard", False):
            continue
        orig_func = getattr(orig_attr, "__func__", orig_attr)
        wrapped = _make_arrival_method_guard(orig_func, mod=mod, clear_tracked=clear_tracked)
        setattr(cls, method_name, wrapped)
        patched = True
        if method_name == "on_warp_finished":
            new_attr = getattr(cls, method_name, wrapped)
            new_func = getattr(new_attr, "__func__", wrapped)
            signal_containers = []
            if mod is not None and hasattr(mod, "warp_signals"):
                signal_containers.append(getattr(mod, "warp_signals"))
            ws_mod = sys.modules.get("eve.client.script.parklife.space_signals.warp_signals")
            if ws_mod is not None and ws_mod not in signal_containers:
                signal_containers.append(ws_mod)
            for ws in signal_containers:
                sig = getattr(ws, "on_warp_finished", None)
                methods_map = getattr(sig, "_methods", None)
                if methods_map is not None and hasattr(methods_map, "items"):
                    for _obj, funcs in list(methods_map.items()):
                        if orig_func in funcs:
                            funcs.discard(orig_func)
                            funcs.add(new_func)
    return patched


def _patch_location_arrival_module(mod):
    if mod is None:
        return False
    patched = False
    for cls_name in (
        "LocationArrivalIndicationService",
        "LocationArrivalIndicationSvc",
    ):
        cls = getattr(mod, cls_name, None)
        if isinstance(cls, type) and _patch_location_arrival_cls(cls, mod=mod):
            patched = True
    orig_helper = getattr(mod, "get_nearest_celestial_to_player", None)
    if (
        orig_helper is not None
        and callable(orig_helper)
        and not isinstance(orig_helper, type)
        and not getattr(orig_helper, "_evomu_camera_guard", False)
    ):
        def _wrapped_helper(*args, **kwargs):
            if _is_docked_or_ballpark_missing(None, mod):
                return None
            try:
                return orig_helper(*args, **kwargs)
            except TypeError as exc:
                if _is_docked_or_ballpark_missing(None, mod) or "NoneType" in str(exc):
                    return None
                raise

        _wrapped_helper._evomu_camera_guard = True
        mod.get_nearest_celestial_to_player = _wrapped_helper
        patched = True
    return patched


def _install_camera_dock_guard():
    """Guard against V24 TacticalCamera NaN/1e35 overflow, dock races (#996/#1051), and HangarView teardown races (#1154)."""
    vss_mod = sys.modules.get("eve.client.script.ui.services.viewStateSvc")
    if vss_mod is not None and "viewStateSvc" not in _HOOKED_CAMERA_GUARD_TARGETS:
        vss_cls = getattr(vss_mod, "ViewStateSvc", None)
        orig_open_view = getattr(vss_cls, "_OpenView", None) if vss_cls is not None else None
        if orig_open_view is not None and not getattr(orig_open_view, "_evomu_camera_guard", False):
            def _wrapped_open_view(self, viewInfo, reopen=False, cameraID=None, **kwargs):
                view_type_enum = getattr(vss_mod, "ViewType", None)
                view_type_primary = getattr(view_type_enum, "Primary", None) if view_type_enum is not None else None
                view_type_secondary = getattr(view_type_enum, "Secondary", None) if view_type_enum is not None else None
                old_view = getattr(self, "primaryInfo", None)
                active_view = getattr(self, "activeViewInfo", None)
                is_secondary_active = (
                    view_type_secondary is not None
                    and active_view is not None
                    and hasattr(active_view, "GetViewType")
                    and active_view.GetViewType() == view_type_secondary
                )
                if (
                    is_secondary_active
                    and view_type_primary is not None
                    and viewInfo is not None
                    and hasattr(viewInfo, "GetViewType")
                    and viewInfo.GetViewType() == view_type_primary
                    and getattr(old_view, "name", None) == "inflight"
                    and getattr(viewInfo, "name", None) != "inflight"
                ):
                    sm = getattr(__builtin__, "sm", None)
                    if sm is not None and hasattr(sm, "IsServiceRunning") and sm.IsServiceRunning("sceneManager"):
                        scene_mgr = sm.GetService("sceneManager")
                        curr_cam = scene_mgr.GetActivePrimaryCamera() if hasattr(scene_mgr, "GetActivePrimaryCamera") else None
                        if curr_cam is not None and getattr(curr_cam, "isActive", False) and hasattr(scene_mgr, "DeactivateFISCamera"):
                            scene_mgr.DeactivateFISCamera()
                _install_hangar_view_teardown_guard()
                _install_hangar_transition_guard()
                return orig_open_view(self, viewInfo, reopen=reopen, cameraID=cameraID, **kwargs)
            _wrapped_open_view._evomu_camera_guard = True
            vss_cls._OpenView = _wrapped_open_view
        orig_activate_view = getattr(vss_cls, "ActivateView", None) if vss_cls is not None else None
        if orig_activate_view is not None and not getattr(orig_activate_view, "_evomu_camera_guard", False):
            def _wrapped_activate_view(self, *args, **kwargs):
                _install_hangar_view_teardown_guard()
                _install_hangar_transition_guard()
                return orig_activate_view(self, *args, **kwargs)
            _wrapped_activate_view._evomu_camera_guard = True
            vss_cls.ActivateView = _wrapped_activate_view
        _HOOKED_CAMERA_GUARD_TARGETS.add("viewStateSvc")

    base_cam_mod = sys.modules.get("eve.client.script.ui.camera.baseCamera")
    if base_cam_mod is not None and "baseCamera" not in _HOOKED_CAMERA_GUARD_TARGETS:
        cam_cls = getattr(base_cam_mod, "Camera", None)
        if cam_cls is not None:
            orig_dir_offset = getattr(cam_cls, "GetLookAtDirectionWithOffset", None)
            if orig_dir_offset is not None and not getattr(orig_dir_offset, "_evomu_camera_guard", False):
                def _wrapped_get_look_at_dir_with_offset(self):
                    _sanitize_camera_state(self)
                    eye_pos = getattr(self, "_eyePosition", (0.0, 500.0, 1000.0))
                    at_pos = getattr(self, "_atPosition", (0.0, 0.0, 0.0))
                    eye_off = getattr(self, "_eyeOffset", None)
                    at_off = getattr(self, "_atOffset", None)
                    if not (
                        _is_finite_safe_vec3(eye_pos)
                        and _is_finite_safe_vec3(at_pos)
                        and (eye_off is None or _is_finite_safe_vec3(eye_off))
                        and (at_off is None or _is_finite_safe_vec3(at_off))
                    ):
                        return _safe_direction_vec3(eye_pos, at_pos)
                    res = orig_dir_offset(self)
                    if not _is_finite_safe_vec3(res, limit=2.0):
                        return _safe_direction_vec3(eye_pos, at_pos)
                    return res
                _wrapped_get_look_at_dir_with_offset._evomu_camera_guard = True
                cam_cls.GetLookAtDirectionWithOffset = _wrapped_get_look_at_dir_with_offset

            orig_dir = getattr(cam_cls, "GetLookAtDirection", None)
            if orig_dir is not None and not getattr(orig_dir, "_evomu_camera_guard", False):
                def _wrapped_get_look_at_dir(self):
                    _sanitize_camera_state(self)
                    eye_pos = getattr(self, "_eyePosition", (0.0, 500.0, 1000.0))
                    zoom_pt = self.GetZoomToPoint() if hasattr(self, "GetZoomToPoint") else getattr(self, "_atPosition", (0.0, 0.0, 0.0))
                    if not (_is_finite_safe_vec3(eye_pos) and _is_finite_safe_vec3(zoom_pt)):
                        return _safe_direction_vec3(eye_pos, getattr(self, "_atPosition", (0.0, 0.0, 0.0)))
                    res = orig_dir(self)
                    if not _is_finite_safe_vec3(res, limit=2.0):
                        return _safe_direction_vec3(eye_pos, zoom_pt)
                    return res
                _wrapped_get_look_at_dir._evomu_camera_guard = True
                cam_cls.GetLookAtDirection = _wrapped_get_look_at_dir
        _HOOKED_CAMERA_GUARD_TARGETS.add("baseCamera")

    tac_mod = sys.modules.get("eve.client.script.ui.camera.tacticalCamera")
    if tac_mod is not None and "tacticalCamera" not in _HOOKED_CAMERA_GUARD_TARGETS:
        tac_cls = getattr(tac_mod, "TacticalCamera", None)
        if tac_cls is not None:
            orig_on_activated = getattr(tac_cls, "OnActivated", None)
            if orig_on_activated is not None and not getattr(orig_on_activated, "_evomu_camera_guard", False):
                def _wrapped_on_activated(self, **kwargs):
                    _sanitize_camera_state(self)
                    last_cam = kwargs.get("lastCamera")
                    if last_cam is not None:
                        _sanitize_camera_state(last_cam)
                    return orig_on_activated(self, **kwargs)
                _wrapped_on_activated._evomu_camera_guard = True
                tac_cls.OnActivated = _wrapped_on_activated

            orig_on_deactivated = getattr(tac_cls, "OnDeactivated", None)
            if orig_on_deactivated is not None and not getattr(orig_on_deactivated, "_evomu_camera_guard", False):
                def _wrapped_on_deactivated(self):
                    res = orig_on_deactivated(self)
                    _sanitize_camera_state(self)
                    return res
                _wrapped_on_deactivated._evomu_camera_guard = True
                tac_cls.OnDeactivated = _wrapped_on_deactivated

            orig_get_new_dir = getattr(tac_cls, "_GetNewDirection", None)
            if orig_get_new_dir is not None and not getattr(orig_get_new_dir, "_evomu_camera_guard", False):
                def _wrapped_get_new_direction(self, direction):
                    if not _is_finite_safe_vec3(direction):
                        return _safe_tactical_new_direction(direction)
                    res = orig_get_new_dir(self, direction)
                    if not _is_finite_safe_vec3(res, limit=2.0):
                        return _safe_tactical_new_direction(direction)
                    return res
                _wrapped_get_new_direction._evomu_camera_guard = True
                tac_cls._GetNewDirection = _wrapped_get_new_direction

            orig_reset_eye_at = getattr(tac_cls, "_ResetEyeAndAtPosition", None)
            if orig_reset_eye_at is not None and not getattr(orig_reset_eye_at, "_evomu_camera_guard", False):
                def _wrapped_reset_eye_and_at(self, eyePos0, atPos0, distance, animate=True, duration=None):
                    _sanitize_camera_state(self)
                    if not _is_finite_safe_vec3(atPos0):
                        atPos0 = list(_vec3_tuple(getattr(self, "default_atPosition", (0.0, 0.0, 0.0)), (0.0, 0.0, 0.0)))
                        animate = False
                    if not _is_finite_safe_vec3(eyePos0):
                        eyePos0 = list(_vec3_tuple(getattr(self, "default_eyePosition", (0.0, 500.0, 1000.0)), (0.0, 500.0, 1000.0)))
                        animate = False
                    if _vec3_tuple(eyePos0, (0.0, 500.0, 1000.0)) == _vec3_tuple(atPos0, (0.0, 0.0, 0.0)):
                        eyePos0 = [atPos0[0], atPos0[1] + 500.0, atPos0[2] + 1000.0]
                        animate = False
                    return orig_reset_eye_at(self, eyePos0, atPos0, distance, animate, duration)
                _wrapped_reset_eye_and_at._evomu_camera_guard = True
                tac_cls._ResetEyeAndAtPosition = _wrapped_reset_eye_and_at

            orig_set_fixed = getattr(tac_cls, "SetAtPosToFixedDistance", None)
            if orig_set_fixed is not None and not getattr(orig_set_fixed, "_evomu_camera_guard", False):
                def _wrapped_set_fixed_dist(self):
                    _sanitize_camera_state(self)
                    return orig_set_fixed(self)
                _wrapped_set_fixed_dist._evomu_camera_guard = True
                tac_cls.SetAtPosToFixedDistance = _wrapped_set_fixed_dist

            orig_set_rot = getattr(tac_cls, "SetAppropriateAtPositionForRotation", None)
            if orig_set_rot is not None and not getattr(orig_set_rot, "_evomu_camera_guard", False):
                def _wrapped_set_appropriate_at_pos(self, atPos):
                    _sanitize_camera_state(self)
                    if not _is_finite_safe_vec3(atPos):
                        atPos = getattr(self, "_atPosition", [0.0, 0.0, 0.0])
                    return orig_set_rot(self, atPos)
                _wrapped_set_appropriate_at_pos._evomu_camera_guard = True
                tac_cls.SetAppropriateAtPositionForRotation = _wrapped_set_appropriate_at_pos
        _HOOKED_CAMERA_GUARD_TARGETS.add("tacticalCamera")

    michelle_mod = sys.modules.get("eve.client.script.remote.michelle")
    if michelle_mod is not None and "michelle" not in _HOOKED_CAMERA_GUARD_TARGETS:
        michelle_cls = getattr(michelle_mod, "Michelle", None)
        if michelle_cls is not None:
            orig_balls_slims = getattr(michelle_cls, "GetBallsAndSlimItems", None)
            if orig_balls_slims is not None and not getattr(orig_balls_slims, "_evomu_camera_guard", False):
                def _wrapped_get_balls_and_slim_items(self, *args, **kwargs):
                    if _michelle_ballpark_missing(self):
                        return []
                    try:
                        res = orig_balls_slims(self, *args, **kwargs)
                    except TypeError as exc:
                        if _michelle_ballpark_missing(self) or "NoneType" in str(exc):
                            return []
                        raise
                    return [] if res is None else res
                _wrapped_get_balls_and_slim_items._evomu_camera_guard = True
                michelle_cls.GetBallsAndSlimItems = _wrapped_get_balls_and_slim_items
        _HOOKED_CAMERA_GUARD_TARGETS.add("michelle")

    for target_key, mod_name in (
        ("mining.locationArrivalIndicationService", "mining.client.location_arrival_indication_service"),
        ("infoPanels.locationArrivalIndicationSvc", "eve.client.script.ui.shared.infoPanels.locationArrivalIndicationSvc"),
    ):
        arr_mod = sys.modules.get(mod_name)
        if arr_mod is not None and target_key not in _HOOKED_CAMERA_GUARD_TARGETS:
            _patch_location_arrival_module(arr_mod)
            _HOOKED_CAMERA_GUARD_TARGETS.add(target_key)

    sm = getattr(__builtin__, "sm", None)
    services = getattr(sm, "services", None) if sm is not None else None
    if isinstance(services, dict):
        for svc_name in ("locationArrivalIndicationSvc", "locationArrivalIndication"):
            svc = services.get(svc_name)
            if svc is not None:
                svc_cls = getattr(svc, "__class__", None)
                svc_mod = sys.modules.get(getattr(svc_cls, "__module__", "")) if svc_cls is not None else None
                if isinstance(svc_cls, type) and svc_cls is not object:
                    _patch_location_arrival_cls(svc_cls, mod=svc_mod)
                if "on_warp_finished" in getattr(svc, "__dict__", {}):
                    inst_fn = svc.__dict__["on_warp_finished"]
                    if callable(inst_fn) and not getattr(inst_fn, "_evomu_camera_guard", False):
                        def _make_inst_guard(fn, inst, mod):
                            def _wrapped_inst(*args, **kwargs):
                                if _is_docked_or_ballpark_missing(inst, mod):
                                    if hasattr(inst, "tracked_slim_item"):
                                        inst.tracked_slim_item = None
                                    return None
                                try:
                                    return fn(*args, **kwargs)
                                except TypeError as exc:
                                    if _is_docked_or_ballpark_missing(inst, mod) or "NoneType" in str(exc):
                                        if hasattr(inst, "tracked_slim_item"):
                                            inst.tracked_slim_item = None
                                        return None
                                    raise
                            _wrapped_inst._evomu_camera_guard = True
                            return _wrapped_inst
                        svc.on_warp_finished = _make_inst_guard(inst_fn, svc, svc_mod)

    _install_hangar_view_teardown_guard()
    _install_hangar_transition_guard()


class _HangarViewTornDown(Exception):
    """Internal sentinel raised when HangarView is cleared mid-yield."""


# Guarded HangarView methods that require an active station/structure dock (#1163).
_HANGAR_VIEW_UNDOCK_METHODS = (
    "GetShipItemFromHangar",
    "SetUpModels",
    "LoadAndSetupScene",
    "LoadView",
    "LoadCamera",
    "ReloadView",
    "ProcessActiveShipChanged",
)


# Guarded HangarView methods: (method_name, abort_replace_on_stale, check_after_call)
_HANGAR_VIEW_TEARDOWN_METHODS = (
    ("_ReplaceExistingShipModelModular", True, True),
    ("RemoveAllShipsFromScene", True, False),
    ("AddModelToScene", True, True),
    ("PlayDockSequence", False, False),
    ("PlaySwitchShipSequence", False, False),
    ("PlayUpdateShipSequence", False, False),
    ("_SetUpLightingRig", False, False),
    ("ConfigureVFXMultiEffect", False, False),
    ("OnActiveShipSkinChange", False, True),
)


def _current_execution_key():
    """Identify the active Stackless tasklet or thread without importing modules."""
    sl = sys.modules.get("stackless")
    if sl is not None and hasattr(sl, "getcurrent"):
        try:
            return ("sl", id(sl.getcurrent()))
        except Exception:
            pass
    th = sys.modules.get("threading")
    if th is not None and hasattr(th, "current_thread"):
        try:
            return ("th", id(th.current_thread()))
        except Exception:
            pass
    return 0


def _is_hangar_view_torn_down(view):
    if view is None:
        return True
    return getattr(view, "shipBehaviour", None) is None or getattr(view, "activeHangarScene", None) is None


def _is_hangar_view_stale(view, expected_gen=None):
    if _is_hangar_view_torn_down(view):
        return True
    curr_gen = getattr(view, "_evomu_hangar_gen", 0)
    if expected_gen is not None and curr_gen != expected_gen:
        return True
    gens_by_key = getattr(view, "_evomu_replace_gens", None)
    if isinstance(gens_by_key, dict):
        stack = gens_by_key.get(_current_execution_key())
        if stack and curr_gen != stack[-1]:
            return True
    return False


def _in_active_hangar_replace(view):
    gens_by_key = getattr(view, "_evomu_replace_gens", None)
    return bool(isinstance(gens_by_key, dict) and gens_by_key.get(_current_execution_key()))


def _abort_hangar_call_or_noop(view, abort_replace):
    if abort_replace and _in_active_hangar_replace(view):
        raise _HangarViewTornDown()
    return None


def _make_hangar_clear_scene_guard(orig_fn):
    def _wrapped_clear_scene(self, *args, **kwargs):
        try:
            return orig_fn(self, *args, **kwargs)
        finally:
            self._evomu_hangar_gen = getattr(self, "_evomu_hangar_gen", 0) + 1

    _wrapped_clear_scene._evomu_camera_guard = True
    return _wrapped_clear_scene


def _make_hangar_replace_guard(orig_fn):
    def _wrapped_replace_existing_ship_model(self, *args, **kwargs):
        if _is_hangar_view_torn_down(self):
            return None
        gens_by_key = getattr(self, "_evomu_replace_gens", None)
        if not isinstance(gens_by_key, dict):
            gens_by_key = {}
            self._evomu_replace_gens = gens_by_key
        key = _current_execution_key()
        stack = gens_by_key.setdefault(key, [])
        gen = getattr(self, "_evomu_hangar_gen", 0)
        stack.append(gen)
        try:
            try:
                return orig_fn(self, *args, **kwargs)
            except _HangarViewTornDown:
                return None
            except AttributeError:
                if _is_hangar_view_stale(self, expected_gen=gen):
                    return None
                raise
        finally:
            stack.pop()
            if not stack:
                gens_by_key.pop(key, None)

    _wrapped_replace_existing_ship_model._evomu_camera_guard = True
    return _wrapped_replace_existing_ship_model


def _make_hangar_teardown_guard(orig_fn, abort_replace=False, check_after=False):
    def _wrapped_hangar_method(self, *args, **kwargs):
        if _is_hangar_view_stale(self):
            return _abort_hangar_call_or_noop(self, abort_replace)
        gen_before = getattr(self, "_evomu_hangar_gen", 0)
        try:
            result = orig_fn(self, *args, **kwargs)
        except AttributeError:
            if _is_hangar_view_stale(self, expected_gen=gen_before):
                return _abort_hangar_call_or_noop(self, abort_replace)
            raise
        if check_after and _is_hangar_view_stale(self, expected_gen=gen_before):
            return _abort_hangar_call_or_noop(self, abort_replace)
        return result

    _wrapped_hangar_method._evomu_camera_guard = True
    return _wrapped_hangar_method


def _is_hangar_session_undocked():
    """Return True when EVE session is loaded and neither docked at station nor structure."""
    sess = getattr(__builtin__, "session", None)
    if sess is None:
        return False
    return getattr(sess, "stationid", None) is None and getattr(sess, "structureid", None) is None


def _make_hangar_undock_guard(orig_fn):
    """Guard HangarView methods against undocking mid-reload / /moveme (#1163)."""
    def _wrapped_hangar_undock_method(self, *args, **kwargs):
        if _is_hangar_session_undocked():
            return None
        try:
            return orig_fn(self, *args, **kwargs)
        except Exception:
            if _is_hangar_session_undocked():
                return None
            raise

    _wrapped_hangar_undock_method._evomu_camera_guard = True
    return _wrapped_hangar_undock_method


def _close_transition_fade_layer(transition):
    """Dismiss HangarToHangar fade overlay cleanly when undocked mid-transition (#1163)."""
    fade_layer = getattr(transition, "fadeLayer", None)
    if fade_layer is not None:
        close_fn = getattr(fade_layer, "Close", None)
        if callable(close_fn):
            close_fn()
        try:
            delattr(transition, "fadeLayer")
        except AttributeError:
            pass
    sm = getattr(__builtin__, "sm", None)
    if sm is not None and hasattr(sm, "IsServiceRunning") and sm.IsServiceRunning("sceneManager"):
        scene_mgr = sm.GetService("sceneManager")
        fade_out = getattr(scene_mgr, "FadeOut", None)
        if callable(fade_out):
            try:
                fade_out(duration=0.0, sleep=False)
            except TypeError:
                fade_out(0.0)


def _call_base_transition_method(base_transition_cls, method_name, transition, from_view, to_view):
    """Invoke a base `Transition` hook safely under both Python 2.7 and Python 3."""
    if base_transition_cls is None or not isinstance(transition, base_transition_cls):
        return None
    fn = getattr(base_transition_cls, method_name, None)
    if callable(fn):
        return fn(transition, from_view, to_view)
    return None


def _install_hangar_transition_guard():
    """Guard HangarToHangarTransition against mid-fade undock / /moveme (#1163)."""
    if "hangarTransition" in _HOOKED_CAMERA_GUARD_TARGETS:
        return False
    trans_mod = sys.modules.get("eve.client.script.ui.view.transitions")
    trans_cls = getattr(trans_mod, "HangarToHangarTransition", None) if trans_mod is not None else None
    if trans_cls is None or trans_cls is object or not (
        isinstance(trans_cls, type) or hasattr(trans_cls, "__bases__")
    ):
        return False

    vss_mod = sys.modules.get("eve.client.script.ui.services.viewStateSvc")
    base_transition_cls = (
        getattr(trans_mod, "Transition", None)
        or (getattr(vss_mod, "Transition", None) if vss_mod is not None else None)
    )

    orig_start = getattr(trans_cls, "StartTransition", None)
    if callable(orig_start) and not getattr(orig_start, "_evomu_camera_guard", False):
        def _wrapped_start_transition(self, fromView, toView):
            _install_hangar_view_teardown_guard()
            if _is_hangar_session_undocked():
                self._evomu_aborted_undocked = True
                hangar_view = getattr(self, "hangarView", None)
                clear_fn = getattr(hangar_view, "ClearScene", None) if hangar_view is not None else None
                if callable(clear_fn):
                    clear_fn()
                _call_base_transition_method(base_transition_cls, "StartTransition", self, fromView, toView)
                return None
            self._evomu_aborted_undocked = False
            try:
                return orig_start(self, fromView, toView)
            except Exception:
                if _is_hangar_session_undocked():
                    self._evomu_aborted_undocked = True
                    return None
                raise
            finally:
                if _is_hangar_session_undocked():
                    self._evomu_aborted_undocked = True

        _wrapped_start_transition._evomu_camera_guard = True
        trans_cls.StartTransition = _wrapped_start_transition

    orig_end = getattr(trans_cls, "EndTransition", None)
    if callable(orig_end) and not getattr(orig_end, "_evomu_camera_guard", False):
        def _wrapped_end_transition(self, fromView, toView):
            if getattr(self, "_evomu_aborted_undocked", False) or _is_hangar_session_undocked():
                _close_transition_fade_layer(self)
                return _call_base_transition_method(base_transition_cls, "EndTransition", self, fromView, toView)
            return orig_end(self, fromView, toView)

        _wrapped_end_transition._evomu_camera_guard = True
        trans_cls.EndTransition = _wrapped_end_transition

    _HOOKED_CAMERA_GUARD_TARGETS.add("hangarTransition")
    return True


def _install_hangar_view_teardown_guard():
    """Guard HangarView ship-model calls against ClearScene/UnloadView teardown races (#1154)."""
    if "hangarView" in _HOOKED_CAMERA_GUARD_TARGETS:
        return False
    hangar_mod = sys.modules.get("eve.client.script.ui.view.hangarView")
    hangar_cls = getattr(hangar_mod, "HangarView", None) if hangar_mod is not None else None
    if hangar_cls is None or hangar_cls is object or not (
        isinstance(hangar_cls, type) or hasattr(hangar_cls, "__bases__")
    ):
        return False

    orig_clear = getattr(hangar_cls, "ClearScene", None)
    if callable(orig_clear) and not getattr(orig_clear, "_evomu_camera_guard", False):
        hangar_cls.ClearScene = _make_hangar_clear_scene_guard(orig_clear)

    orig_replace = getattr(hangar_cls, "ReplaceExistingShipModel", None)
    if callable(orig_replace) and not getattr(orig_replace, "_evomu_camera_guard", False):
        hangar_cls.ReplaceExistingShipModel = _make_hangar_replace_guard(orig_replace)

    for method_name, abort_replace, check_after in _HANGAR_VIEW_TEARDOWN_METHODS:
        orig_fn = getattr(hangar_cls, method_name, None)
        if callable(orig_fn) and not getattr(orig_fn, "_evomu_camera_guard", False):
            setattr(
                hangar_cls,
                method_name,
                _make_hangar_teardown_guard(orig_fn, abort_replace=abort_replace, check_after=check_after),
            )

    for method_name in _HANGAR_VIEW_UNDOCK_METHODS:
        orig_fn = getattr(hangar_cls, method_name, None)
        if callable(orig_fn) and not getattr(orig_fn, "_evomu_camera_guard", False):
            setattr(hangar_cls, method_name, _make_hangar_undock_guard(orig_fn))

    _HOOKED_CAMERA_GUARD_TARGETS.add("hangarView")
    return True


def _maybe_schedule_driver_on_main_thread():
    global _DRIVER_SCHEDULED
    if _DRIVER_SCHEDULED:
        return
    if "uthread" not in sys.modules:
        return
    sm = getattr(__builtin__, "sm", None)
    if sm is None or not hasattr(sm, "IsServiceRunning"):
        return
    try:
        if not sm.IsServiceRunning("machoNet"):
            return
    except Exception:
        return
    _install_flatbuffer_cdn_http_guard()
    _install_fsd_typelist_preload_hook()
    _install_camera_dock_guard()
    _DRIVER_SCHEDULED = True
    try:
        if os.environ.get("EVOMU_PLAYTESTER_PORT") or os.environ.get("EVOMU_PLAYTESTER_CFG"):
            import evomu_playtester_hook
            evomu_playtester_hook.schedule_startup()
        else:
            import evomu_inclient_driver
            evomu_inclient_driver.schedule_startup()
    except Exception:
        _log_exc_line("schedule_startup failed:\n%s" % traceback.format_exc())


class _EvomuMetaHook(object):
    def find_module(self, fullname, path=None):
        _install_logmodule_hook()
        _install_xmpp_plaintext_guard()
        _install_xmpp_port_guard()
        _install_flatbuffer_cdn_http_guard()
        _install_fsd_typelist_preload_hook()
        _install_camera_dock_guard()
        _maybe_schedule_driver_on_main_thread()
        return None


def install():
    global _META_HOOK_INSTALLED
    if _META_HOOK_INSTALLED:
        return
    _META_HOOK_INSTALLED = True
    sys.meta_path.insert(0, _EvomuMetaHook())
