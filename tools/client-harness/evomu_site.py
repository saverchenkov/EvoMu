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
import re
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
_EXC_LINE_RE = re.compile(
    r"^[A-Za-z_][A-Za-z0-9_.]*(?:Error|Exception|Warning|Exit|Interrupt|Iteration|Failure)(?:\s*:.*)?$"
)
_CAMERA_MAX_SAFE_COORD = 1e16


def _is_exc_noise_line(ln):
    if not ln:
        return True
    if ln == "EXCEPTION END" or ln == "Traceback (most recent call last):":
        return True
    if ln.startswith(("EXCEPTION #", "Common path prefix =", "Reported from", "Local variables", "Thread Locals")):
        return True
    if "evomu_site.py" in ln or "logmodule/__init__.py" in ln:
        return True
    return False


def _log_exc_line(msg):
    global _EXC_SEQ
    _EXC_SEQ += 1
    try:
        lines = [ln.strip() for ln in str(msg).splitlines() if ln.strip()]
        non_noise = [ln for ln in lines if not _is_exc_noise_line(ln)]
        headline = non_noise[-1] if non_noise else "Client Python exception"
        for ln in reversed(non_noise):
            if _EXC_LINE_RE.match(ln) or "Error:" in ln or "Exception:" in ln:
                headline = ln
                break
        msg_txt = str(msg)
        if len(msg_txt) > 4000:
            msg_txt = msg_txt[:1400] + "\n... [truncated] ...\n" + msg_txt[-2500:]
        _RECENT_EXCEPTIONS.append({
            "seq": _EXC_SEQ,
            "ts": round(time.time(), 3),
            "headline": headline[:200],
            "trace": msg_txt,
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
                if attr_name == "LogTraceback" and (
                    kwargs.get("severity") == 1
                    or (kwargs.get("severity") in (1, 2) and sys.exc_info()[0] is None)
                ):
                    return orig(*args, **kwargs)
                tb = "None\n" if attr_name == "LogTraceback" else traceback.format_exc()
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


def _get_bdl_cached_data(cls):
    for attr in ("__binary__", "_BuiltDataLoader__binary__", "_BuiltDataLoader__data"):
        val = getattr(cls, attr, None)
        if val is not None:
            return val
    return None


def _set_bdl_cached_data(cls, data):
    if data is None:
        return
    if hasattr(cls, "__binary__") or (
        not hasattr(cls, "_BuiltDataLoader__binary__") and not hasattr(cls, "_BuiltDataLoader__data")
    ):
        try:
            setattr(cls, "__binary__", data)
            return
        except Exception:
            pass
    if hasattr(cls, "_BuiltDataLoader__binary__"):
        try:
            setattr(cls, "_BuiltDataLoader__binary__", data)
            return
        except Exception:
            pass
    try:
        setattr(cls, "_BuiltDataLoader__data", data)
    except Exception:
        pass


def _call_cls_method_zero_or_false(fn):
    try:
        return fn()
    except TypeError:
        try:
            return fn(False)
        except Exception:
            return None
    except Exception:
        return None


def _invoke_bdl_load_binary(cls, bdl_mod=None):
    load_bin = getattr(cls, "_BuiltDataLoader__LoadBinary", None)
    if not callable(load_bin):
        return _get_bdl_cached_data(cls)
    data = _call_cls_method_zero_or_false(load_bin)
    if data is None and bdl_mod is not None:
        use_res_fn = getattr(bdl_mod, "UseResFileServer", None)
        disable_fn = getattr(bdl_mod, "DisableUseResFileServer", None)
        enable_fn = getattr(bdl_mod, "EnableUseResFileServer", None)
        try:
            was_enabled = bool(callable(use_res_fn) and use_res_fn())
        except Exception:
            was_enabled = False
        if was_enabled and callable(disable_fn):
            try:
                disable_fn()
                data = _call_cls_method_zero_or_false(load_bin)
            finally:
                if callable(enable_fn):
                    try:
                        enable_fn()
                    except Exception:
                        pass
    if data is not None:
        _set_bdl_cached_data(cls, data)
    return _get_bdl_cached_data(cls)


def _install_fsd_typelist_preload_hook(preload_typelist=True):
    """Ensure BuiltDataLoader.GetData never returns None when asynchronous loading
    has not finished yet, and eagerly pre-load evetypes.data.TypeListLoader (#796, #797).

    In V24.01, TypeListLoader lives in `evetypes.data`, `BuiltDataLoader.__LoadBinary(cls)`
    takes no extra boolean argument, and loaded data is stored in `_BuiltDataLoader__binary__`.
    Pass `preload_typelist=False` from `sys.meta_path` `find_module` to avoid doing ResFile
    I/O or yielding while holding Python's import lock.
    """
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
                    orig_fn = getattr(orig_get_data, "im_func", None) or getattr(orig_get_data, "__func__", None)
                    try:
                        data = orig_fn(cls) if callable(orig_fn) else orig_get_data()
                    except Exception:
                        data = None
                    if data is None:
                        data = _invoke_bdl_load_binary(cls, bdl_mod=bdl_mod)
                    return data
                try:
                    bdl_cls.GetData = classmethod(_evomu_safe_get_data)
                    bdl_cls._evomu_getdata_patched = True
                except Exception:
                    pass
    if not preload_typelist:
        return
    if "evetypes.data" not in sys.modules and "blue" in sys.modules:
        try:
            __import__("evetypes.data", fromlist=["TypeListLoader"])
        except Exception:
            pass
    for mod_name in ("evetypes.data", "evetypes", "evetypes.storages"):
        et_mod = sys.modules.get(mod_name)
        if et_mod is None:
            continue
        tll = getattr(et_mod, "TypeListLoader", None)
        if tll is None:
            continue
        try:
            if _get_bdl_cached_data(tll) is None:
                get_data_fn = getattr(tll, "GetData", None)
                if callable(get_data_fn):
                    get_data_fn()
            if _get_bdl_cached_data(tll) is None:
                _invoke_bdl_load_binary(tll, bdl_mod=bdl_mod)
            if _get_bdl_cached_data(tll) is not None:
                _HOOKED_TYPELIST_PRELOAD = True
                break
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


def _clear_camera_anchor_state(cam):
    """Clear anchor ball/offset references on BaseSpaceCamera teardown/poison (#1340)."""
    for attr in ("_anchorBall", "_anchorOffset"):
        try:
            setattr(cam, attr, None)
        except AttributeError:
            pass


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
    for attr in (
        "_eyeOffset",
        "_atOffset",
        "_eyeTransitOffset",
        "_atTransitOffset",
        "_eyeAndAtOffset",
        "_anchorOffset",
    ):
        val = getattr(cam, attr, None)
        if val is not None and not _is_finite_safe_vec3(val):
            setattr(cam, attr, None)
            if attr == "_anchorOffset":
                _clear_camera_anchor_state(cam)
            changed = True
    return changed


def _sanitize_present_camera_state(cam):
    if hasattr(cam, "_eyePosition") or hasattr(cam, "_atPosition"):
        _sanitize_camera_state(cam)


def _sanitize_base_space_anchor_state(cam):
    anchor_offset = getattr(cam, "_anchorOffset", None)
    if anchor_offset is not None and not _is_finite_safe_vec3(anchor_offset):
        _clear_camera_anchor_state(cam)
    _sanitize_present_camera_state(cam)


def _resolve_base_space_get_ball_position(orig_func):
    fn_globals = getattr(orig_func, "__globals__", None) or getattr(orig_func, "func_globals", None)
    get_ball_pos = fn_globals.get("GetBallPosition") if isinstance(fn_globals, dict) else None
    if not callable(get_ball_pos):
        bsc_mod = sys.modules.get("eve.client.script.ui.camera.baseSpaceCamera")
        get_ball_pos = getattr(bsc_mod, "GetBallPosition", None) if bsc_mod is not None else None
    return get_ball_pos if callable(get_ball_pos) else None


def _make_reset_anchor_pos_guard(orig_fn):
    """Clear non-finite/_CAMERA_MAX_SAFE_COORD _anchorOffset before BaseSpaceCamera.ResetAnchorPos adds it (#1340)."""
    orig_func = getattr(orig_fn, "__func__", orig_fn)

    def _wrapped_reset_anchor_pos(self):
        _sanitize_base_space_anchor_state(self)
        anchor_offset = getattr(self, "_anchorOffset", None)
        if anchor_offset:
            eye_pos = getattr(self, "eyePosition", None)
            at_pos = getattr(self, "atPosition", None)
            if (
                not _is_finite_safe_vec3(anchor_offset)
                or not _is_finite_safe_vec3(eye_pos)
                or not _is_finite_safe_vec3(at_pos)
            ):
                _clear_camera_anchor_state(self)
        return orig_func(self)

    _wrapped_reset_anchor_pos.__name__ = getattr(orig_func, "__name__", "ResetAnchorPos")
    _wrapped_reset_anchor_pos._evomu_camera_guard = True
    return _wrapped_reset_anchor_pos


def _make_update_anchor_offset_guard(orig_fn):
    """Clear stale _anchorBall/_anchorOffset if GetBallPosition returns non-finite or > _CAMERA_MAX_SAFE_COORD (#1340)."""
    orig_func = getattr(orig_fn, "__func__", orig_fn)

    def _wrapped_update_anchor_offset(self):
        _sanitize_base_space_anchor_state(self)
        anchor_ball = getattr(self, "_anchorBall", None)
        if anchor_ball:
            if hasattr(anchor_ball, "ballpark") and getattr(anchor_ball, "ballpark", None) is None:
                _clear_camera_anchor_state(self)
            else:
                get_ball_pos = _resolve_base_space_get_ball_position(orig_func)
                if get_ball_pos is not None:
                    pos = get_ball_pos(anchor_ball)
                    if not _is_finite_safe_vec3(pos):
                        _clear_camera_anchor_state(self)
        try:
            res = orig_func(self)
        except Exception:
            cur_offset = getattr(self, "_anchorOffset", None)
            if cur_offset is not None and not _is_finite_safe_vec3(cur_offset):
                _clear_camera_anchor_state(self)
                _sanitize_present_camera_state(self)
                return None
            raise
        _sanitize_base_space_anchor_state(self)
        return res

    _wrapped_update_anchor_offset.__name__ = getattr(orig_func, "__name__", "UpdateAnchorOffset")
    _wrapped_update_anchor_offset._evomu_camera_guard = True
    return _wrapped_update_anchor_offset


def _make_get_track_position_guard(orig_fn):
    """Prevent poisoned _anchorOffset or GetBallPosition from poisoning BaseSpaceCamera.GetTrackPosition (#1340)."""
    orig_func = getattr(orig_fn, "__func__", orig_fn)

    def _wrapped_get_track_position(self, *args, **kwargs):
        ball = args[0] if args else kwargs.get("ball")
        _sanitize_base_space_anchor_state(self)
        res = orig_func(self, *args, **kwargs)
        if not _is_finite_safe_vec3(res):
            cur_offset = getattr(self, "_anchorOffset", None)
            if (cur_offset is not None and not _is_finite_safe_vec3(cur_offset)) or (
                ball is not None and ball is getattr(self, "_anchorBall", None)
            ):
                _clear_camera_anchor_state(self)
            _sanitize_present_camera_state(self)
            at_pos = getattr(self, "_atPosition", getattr(self, "atPosition", (0.0, 0.0, 0.0)))
            return tuple(_vec3_tuple(at_pos, (0.0, 0.0, 0.0)))
        return res

    _wrapped_get_track_position.__name__ = getattr(orig_func, "__name__", "GetTrackPosition")
    _wrapped_get_track_position._evomu_camera_guard = True
    return _wrapped_get_track_position


def _install_base_space_camera_guard():
    """Guard BaseSpaceCamera anchor offset/position teardown against -FLT_MAX / non-finite balls (#1340)."""
    return _wrap_cls_methods_table(
        "baseSpaceCamera",
        "eve.client.script.ui.camera.baseSpaceCamera",
        "BaseSpaceCamera",
        (
            ("ResetAnchorPos", _make_reset_anchor_pos_guard),
            ("UpdateAnchorOffset", _make_update_anchor_offset_guard),
            ("GetTrackPosition", _make_get_track_position_guard),
        ),
    )


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
                _install_skill_plan_panel_signal_guard()
                return orig_open_view(self, viewInfo, reopen=reopen, cameraID=cameraID, **kwargs)
            _wrapped_open_view._evomu_camera_guard = True
            vss_cls._OpenView = _wrapped_open_view
        orig_activate_view = getattr(vss_cls, "ActivateView", None) if vss_cls is not None else None
        if orig_activate_view is not None and not getattr(orig_activate_view, "_evomu_camera_guard", False):
            def _wrapped_activate_view(self, *args, **kwargs):
                _install_hangar_view_teardown_guard()
                _install_hangar_transition_guard()
                _install_skill_plan_panel_signal_guard()
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

    _install_base_space_camera_guard()

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
    _install_live_client_sync_guards()


class _HangarViewTornDown(Exception):
    """Internal sentinel raised when HangarView is cleared mid-yield."""


# Guarded HangarView methods that require an active station/structure dock (#1163).
# ReloadView uses _make_hangar_reload_guard below to track in-flight reloads too (#1286).
_HANGAR_VIEW_UNDOCK_METHODS = (
    "GetShipItemFromHangar",
    "SetUpModels",
    "LoadAndSetupScene",
    "LoadView",
    "LoadCamera",
    "ProcessActiveShipChanged",
)


# Guarded HangarView methods: (method_name, abort_replace_on_stale, check_after_call)
# OnActiveShipSkinChange uses _make_hangar_skin_change_guard for reload/stale-ship checks (#1286).
_HANGAR_VIEW_TEARDOWN_METHODS = (
    ("_ReplaceExistingShipModelModular", True, True),
    ("RemoveAllShipsFromScene", True, False),
    ("AddModelToScene", True, True),
    ("PlayDockSequence", False, False),
    ("PlaySwitchShipSequence", False, False),
    ("PlayUpdateShipSequence", False, False),
    ("_SetUpLightingRig", False, False),
    ("ConfigureVFXMultiEffect", False, False),
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


def _is_none_clear_refs_error(exc):
    return "'NoneType' object has no attribute 'ClearRefs'" in str(exc)


def _make_hangar_clear_scene_guard(orig_fn):
    def _wrapped_clear_scene(self, *args, **kwargs):
        _install_modular_hangar_teardown_guard()
        _install_modular_capital_hangar_guard()
        _install_modular_hangar_camera_guard()
        _install_base_hangar_behaviour_guard()
        try:
            return orig_fn(self, *args, **kwargs)
        except AttributeError as exc:
            if _is_none_clear_refs_error(exc) or _is_hangar_view_torn_down(self):
                return None
            raise
        finally:
            self._evomu_hangar_gen = getattr(self, "_evomu_hangar_gen", 0) + 1
            self._evomu_ship_retry_pending = False
            self._evomu_ship_retry_count = 0

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


def _session_ship_id():
    sess = getattr(__builtin__, "session", None)
    return getattr(sess, "shipid", None) if sess is not None else None


def _is_hangar_skin_change_stale(view, args, kwargs):
    """Return True during HangarView reloads or while shipBehaviour still tracks a prior ship (#1286)."""
    if getattr(view, "_evomu_hangar_reload_pending", False):
        return True
    item_id = args[0] if args else kwargs.get("itemID")
    if item_id is not None:
        sess_ship = _session_ship_id()
        if sess_ship is not None and item_id != sess_ship:
            return True
        ship_behaviour = getattr(view, "shipBehaviour", None)
        if ship_behaviour is not None and hasattr(ship_behaviour, "playerShipItemID"):
            if getattr(ship_behaviour, "playerShipItemID", None) != item_id:
                return True
    return False


def _make_hangar_skin_change_guard(orig_fn):
    """Guard HangarView.OnActiveShipSkinChange against teardown and stale ship-switch races (#1154, #1286)."""
    teardown_wrapped = _make_hangar_teardown_guard(orig_fn, abort_replace=False, check_after=True)

    def _wrapped_on_active_ship_skin_change(self, *args, **kwargs):
        if _is_hangar_skin_change_stale(self, args, kwargs):
            return None
        return teardown_wrapped(self, *args, **kwargs)

    _wrapped_on_active_ship_skin_change._evomu_camera_guard = True
    return _wrapped_on_active_ship_skin_change


def _make_hangar_reload_guard(orig_fn):
    """Mark HangarView reload pending until ActivateView/ReloadView finishes while keeping undock protection (#1163, #1286)."""
    undock_wrapped = _make_hangar_undock_guard(orig_fn)

    def _wrapped_reload_view(self, *args, **kwargs):
        if _is_hangar_session_undocked():
            return None
        self._evomu_hangar_reload_pending = True
        try:
            return undock_wrapped(self, *args, **kwargs)
        finally:
            self._evomu_hangar_reload_pending = False

    _wrapped_reload_view._evomu_camera_guard = True
    return _wrapped_reload_view


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
            wrapped_fn = orig_fn
            if method_name == "_ReplaceExistingShipModelModular":
                wrapped_fn = _make_modular_replace_guard(wrapped_fn)
            setattr(
                hangar_cls,
                method_name,
                _make_hangar_teardown_guard(wrapped_fn, abort_replace=abort_replace, check_after=check_after),
            )

    for method_name in _HANGAR_VIEW_UNDOCK_METHODS:
        _wrap_cls_method_if_present(hangar_cls, method_name, _make_hangar_undock_guard)

    _wrap_cls_method_if_present(hangar_cls, "OnActiveShipSkinChange", _make_hangar_skin_change_guard)
    _wrap_cls_method_if_present(hangar_cls, "ReloadView", _make_hangar_reload_guard)
    _install_modular_hangar_camera_guard()

    _HOOKED_CAMERA_GUARD_TARGETS.add("hangarView")
    return True


class _NullHangarCamera(object):
    """Minimal stand-in when hangar GetCamera() returns None during teardown or placement (#1218, #1318)."""

    def ClearRefs(self):
        return None

    def AnimEnterHangar(self, *args, **kwargs):
        return None

    def SetShip(self, *args, **kwargs):
        return None

    def SetCurrentShipSizeClass(self, *args, **kwargs):
        return None

    def SetIsChangingSkin(self, *args, **kwargs):
        return None

    def IsPointWithinViewAngle(self, *args, **kwargs):
        return True


def _begin_null_camera_override(obj):
    get_cam = getattr(obj, "GetCamera", None)
    cam_val = None
    override_cam = False
    if callable(get_cam):
        try:
            cam_val = get_cam()
        except Exception:
            cam_val = None
        if cam_val is None:
            obj.GetCamera = lambda: _NullHangarCamera()
            override_cam = True
    return override_cam, cam_val


def _end_null_camera_override(obj, override_cam):
    if override_cam and isinstance(getattr(obj, "__dict__", None), dict):
        obj.__dict__.pop("GetCamera", None)


def _maybe_populate_empty_modular_docks(view):
    """Ensure docks exist after scene application when background LoadView deferred them (#1318)."""
    sb = getattr(view, "shipBehaviour", None)
    if sb is None:
        return
    small_docks = getattr(sb, "smallDocks", None)
    big_docks = getattr(sb, "bigDocks", None)
    if small_docks is None or big_docks is None or small_docks or big_docks:
        return
    hangar_model = getattr(view, "activeHangarModel", None)
    hangar_scene = getattr(view, "activeHangarScene", None)
    set_anchor = getattr(sb, "SetAnchorPoint", None)
    if hangar_model is not None and hangar_scene is not None and callable(set_anchor):
        set_anchor(hangar_model, hangar_scene)


def _ensure_hangar_faded_in(view):
    if getattr(view, "_starting_boarding_moment", False):
        return
    if getattr(view, "_retriggerableFadeFromBlackTasklet", None) is not None:
        return
    fade_fn = getattr(view, "_FadeFromBlackHandler", None)
    if callable(fade_fn):
        fade_fn(0.5, False)


def _schedule_modular_ship_retry(view, event_type, item_id, type_id):
    """Schedule a bounded one-shot retry if modular PlaceShip left activeShipModel as None (#1318)."""
    if not item_id or not type_id:
        return
    if getattr(view, "_evomu_ship_retry_count", 0) >= 2 or getattr(view, "_evomu_ship_retry_pending", False):
        return
    hangar_mod = sys.modules.get("eve.client.script.ui.view.hangarView")
    replace_update = getattr(hangar_mod, "REPLACE_UPDATE", None) if hangar_mod is not None else None
    replace_switch = getattr(hangar_mod, "REPLACE_SWITCHSHIPS", None) if hangar_mod is not None else None
    allowed = tuple(val for val in (replace_update, replace_switch) if val is not None)
    if allowed and event_type not in allowed:
        return
    uthread2 = sys.modules.get("uthread2")
    delay_fn = getattr(uthread2, "call_after_simtime_delay", None) if uthread2 is not None else None
    if not callable(delay_fn):
        return

    def _retry_place_ship():
        view._evomu_ship_retry_pending = False
        if _is_hangar_view_stale(view) or _is_hangar_session_undocked():
            return None
        if getattr(view, "activeShipModel", None) is not None:
            view._evomu_ship_retry_count = 0
            return None
        get_item = getattr(view, "GetActiveShipItemID", None)
        curr_item = get_item() if callable(get_item) else item_id
        if not curr_item or curr_item != item_id:
            return None
        view._evomu_ship_retry_count = getattr(view, "_evomu_ship_retry_count", 0) + 1
        replace_fn = getattr(view, "ReplaceExistingShipModel", None)
        if callable(replace_fn):
            return replace_fn(event_type, fade=False, skipAnimation=True)
        return None

    view._evomu_ship_retry_pending = True
    try:
        delay_fn(_retry_place_ship, 0.5)
    except Exception:
        view._evomu_ship_retry_pending = False


def _make_modular_replace_guard(orig_fn):
    """Guard HangarView._ReplaceExistingShipModelModular against unready docks/camera and fade-black hangs (#1318)."""
    def _wrapped_modular_replace(self, *args, **kwargs):
        _install_modular_hangar_camera_guard()
        _maybe_populate_empty_modular_docks(self)
        override_cam, _ = _begin_null_camera_override(self)
        try:
            res = orig_fn(self, *args, **kwargs)
        finally:
            _end_null_camera_override(self, override_cam)
        if not _is_hangar_view_stale(self) and not _is_hangar_session_undocked():
            if getattr(self, "activeShipModel", None) is not None:
                self._evomu_ship_retry_count = 0
            else:
                _ensure_hangar_faded_in(self)
                event_type = args[0] if args else kwargs.get("eventType")
                item_id = args[1] if len(args) > 1 else kwargs.get("itemID")
                type_id = args[2] if len(args) > 2 else kwargs.get("typeID")
                if not item_id and callable(getattr(self, "GetActiveShipItemID", None)):
                    item_id = self.GetActiveShipItemID()
                if not type_id and callable(getattr(self, "GetActiveShipTypeID", None)):
                    type_id = self.GetActiveShipTypeID()
                _schedule_modular_ship_retry(self, event_type, item_id, type_id)
        return res

    _wrapped_modular_replace._evomu_camera_guard = True
    return _wrapped_modular_replace


_CONTAINER_INACCESSIBLE_ERROR_KEYS = (
    "InventoryNotAccessible",
    "CantOpenCargoContainer",
    "OutOfRange",
    "ContainerIsLocked",
    "ItemNotFound",
    "FakeItemNotFound",
    "NotInSameSolarSystem",
)
_GENERIC_USER_ERROR_KEYS = ("", "CustomInfo", "RpcError", "ServerError")
_TEXT_TYPES = (str, type(u""))


def _matches_user_error_keys(exc, keys):
    """Return True if *exc* matches one of *keys* without masking unrelated UserErrors."""
    if exc is None:
        return False
    msg = getattr(exc, "msg", None)
    if msg in keys:
        return True
    if isinstance(msg, _TEXT_TYPES) and msg not in _GENERIC_USER_ERROR_KEYS:
        return False
    err_dict = getattr(exc, "dict", None)
    if isinstance(err_dict, dict):
        dict_repr = repr(err_dict)
        if any(key in dict_repr for key in keys):
            return True
    msg_repr = repr(exc)
    if any(key in msg_repr for key in keys):
        return True
    err_str = str(exc)
    return any(key in err_str for key in keys)


def _is_inventory_not_accessible_error(exc):
    """Return True if *exc* represents an InventoryNotAccessible UserError or stringified message."""
    return _matches_user_error_keys(exc, ("InventoryNotAccessible",))


def _is_user_error_exception(exc):
    """Return True if *exc* is an eveexceptions.UserError instance (#1290, #1301)."""
    if exc is None:
        return False
    eve_exc_mod = sys.modules.get("eveexceptions")
    user_error_cls = getattr(eve_exc_mod, "UserError", None) if eve_exc_mod is not None else None
    if isinstance(user_error_cls, type) and isinstance(exc, user_error_cls):
        return True
    return type(exc).__name__ == "UserError" and hasattr(exc, "msg") and hasattr(exc, "dict")


def _present_user_error_dialog(exc, inv_mod=None):
    """Display a client UserError dialog cleanly without escaping into TaskletExt LogException (#1290, #1301)."""
    msg = getattr(exc, "msg", None)
    if not msg:
        args = getattr(exc, "args", ()) or ()
        msg = args[0] if args else None
    if not msg:
        return False
    err_dict = getattr(exc, "dict", None)
    for mod, attr in (
        (inv_mod, "prompt_player_fsd_dialog"),
        (sys.modules.get("player_messaging.client.ui_message"), "prompt_player_fsd_dialog"),
        (sys.modules.get("logmodule"), "uiMessageFunc"),
        (getattr(__builtin__, "eve", None), "Message"),
    ):
        dialog_fn = getattr(mod, attr, None) if mod is not None else None
        if callable(dialog_fn):
            try:
                dialog_fn(msg, err_dict)
                return True
            except Exception:
                continue
    return False


def _is_container_inaccessible_error(exc):
    """Return True if *exc* indicates an opened container/inventory is no longer accessible."""
    return _matches_user_error_keys(exc, _CONTAINER_INACCESSIBLE_ERROR_KEYS)


def _is_stale_inventory_controller_for_session(inv_cont, sess):
    """Return True when *inv_cont*'s controller targets a station/structure unreachable in *sess* (#1267)."""
    if sess is None or inv_cont is None:
        return False
    ctrl = getattr(inv_cont, "invController", None)
    if ctrl is None:
        return False
    ctrl_name = getattr(getattr(ctrl, "__class__", None), "__name__", "")
    sess_station = getattr(sess, "stationid", None)
    sess_struct = getattr(sess, "structureid", None)
    sess_ship = getattr(sess, "shipid", None)
    if sess_station is None and sess_struct is None and (
        "Structure" in ctrl_name or "Station" in ctrl_name
    ):
        return True
    ctrl_item_id = getattr(ctrl, "itemID", None)
    if ctrl_item_id is not None and (
        "Structure" in ctrl_name or "Station" in ctrl_name
    ):
        return ctrl_item_id not in (sess_ship, sess_struct, sess_station)
    return False


def _install_inventory_transition_guard():
    """Guard V24 Inventory/InvContCapacityGauge/invCacheContainer across transitions (#1267, #1333)."""
    changed = False
    inv_mod = sys.modules.get("eve.client.script.ui.shared.inventory.invWindow")
    inv_cls = getattr(inv_mod, "Inventory", None) if inv_mod is not None else None
    if (
        inv_cls is not None
        and "invWindow" not in _HOOKED_CAMERA_GUARD_TARGETS
        and (isinstance(inv_cls, type) or hasattr(inv_cls, "__bases__"))
    ):
        orig_valid = getattr(inv_cls, "_IsCurrentInventoryValid", None)
        if callable(orig_valid) and not getattr(orig_valid, "_evomu_camera_guard", False):
            def _wrapped_is_current_inventory_valid(self, *args, **kwargs):
                sess = getattr(__builtin__, "session", None)
                if (
                    sess is not None
                    and getattr(sess, "structureid", None) is None
                    and getattr(sess, "stationid", None) is None
                ):
                    curr_inv_id = getattr(self, "currInvID", None)
                    win_id = str(getattr(self, "default_windowID", "") or "")
                    if win_id.startswith(
                        ("InventoryStructure", "InventoryStation", "StructureItems", "StructureShips")
                    ):
                        return False
                    if curr_inv_id:
                        inv_head = (
                            str(curr_inv_id[0])
                            if isinstance(curr_inv_id, (tuple, list)) and curr_inv_id
                            else str(curr_inv_id)
                        )
                        if inv_head.startswith(("Structure", "Station")):
                            return False
                    if _is_stale_inventory_controller_for_session(getattr(self, "invCont", None), sess):
                        return False
                try:
                    return orig_valid(self, *args, **kwargs)
                except Exception as exc:
                    if isinstance(exc, (KeyError, RuntimeError)) or _is_container_inaccessible_error(exc):
                        return False
                    raise

            _wrapped_is_current_inventory_valid._evomu_camera_guard = True
            inv_cls._IsCurrentInventoryValid = _wrapped_is_current_inventory_valid

        orig_on_sess = getattr(inv_cls, "OnSessionChanged", None)
        if callable(orig_on_sess) and not getattr(orig_on_sess, "_evomu_camera_guard", False):
            def _wrapped_on_session_changed(self, isRemote, sess, change):
                res = orig_on_sess(self, isRemote, sess, change)
                valid_fn = getattr(self, "_IsCurrentInventoryValid", None)
                if getattr(self, "currInvID", None) is None or (
                    callable(valid_fn) and not valid_fn()
                ):
                    self.updateSelectedItemsPending = None
                return res

            _wrapped_on_session_changed._evomu_camera_guard = True
            inv_cls.OnSessionChanged = _wrapped_on_session_changed

        orig_update = getattr(inv_cls, "_UpdateSelectedItems", None)
        if callable(orig_update) and not getattr(orig_update, "_evomu_camera_guard", False):
            def _wrapped_update_selected_items(self):
                sess = getattr(__builtin__, "session", None)
                if getattr(self, "destroyed", False) or getattr(sess, "mutating", False):
                    self.updateSelectedItemsPending = None
                    return None
                valid_fn = getattr(self, "_IsCurrentInventoryValid", None)
                if callable(valid_fn) and not valid_fn():
                    self.updateSelectedItemsPending = None
                    return None
                inv_cont = getattr(self, "invCont", None)
                if _is_stale_inventory_controller_for_session(inv_cont, sess):
                    self.updateSelectedItemsPending = None
                    return None
                try:
                    return orig_update(self)
                except Exception as exc:
                    if (
                        getattr(self, "destroyed", False)
                        or getattr(sess, "mutating", False)
                        or _is_container_inaccessible_error(exc)
                        and (
                            (callable(valid_fn) and not valid_fn())
                            or _is_stale_inventory_controller_for_session(inv_cont, sess)
                        )
                    ):
                        self.updateSelectedItemsPending = None
                        return None
                    raise

            _wrapped_update_selected_items._evomu_camera_guard = True
            inv_cls._UpdateSelectedItems = _wrapped_update_selected_items

        # V24 OnTreeViewDropData launches StartTasklet(self._MoveItems, entry, nodes).
        # BaseInvContainer._AddItem re-raises NotEnoughCargoSpace when free < 0 (already-overfilled hold),
        # so catch domain UserErrors here and show the FSD dialog instead of triggering TaskletExt LogException (#1290, #1301).
        orig_move_items = getattr(inv_cls, "_MoveItems", None)
        if callable(orig_move_items) and not getattr(orig_move_items, "_evomu_camera_guard", False):
            def _wrapped_move_items(self, *args, **kwargs):
                try:
                    return orig_move_items(self, *args, **kwargs)
                except Exception as exc:
                    if not _is_user_error_exception(exc):
                        raise
                    if not _present_user_error_dialog(exc, inv_mod=inv_mod):
                        raise
                    return None

            _wrapped_move_items._evomu_camera_guard = True
            inv_cls._MoveItems = _wrapped_move_items

        _HOOKED_CAMERA_GUARD_TARGETS.add("invWindow")
        changed = True

    cont_mod = sys.modules.get("eve.client.script.ui.shared.container")
    gauge_cls = getattr(cont_mod, "InvContCapacityGauge", None) if cont_mod is not None else None
    if (
        gauge_cls is not None
        and "invContainer" not in _HOOKED_CAMERA_GUARD_TARGETS
        and (isinstance(gauge_cls, type) or hasattr(gauge_cls, "__bases__"))
    ):
        for meth_name in ("SetSecondaryVolume", "UpdateLabel"):
            orig_meth = getattr(gauge_cls, meth_name, None)
            if callable(orig_meth) and not getattr(orig_meth, "_evomu_camera_guard", False):
                def _make_gauge_guard(orig_fn):
                    def _wrapped_gauge_method(self, *args, **kwargs):
                        sess = getattr(__builtin__, "session", None)
                        inv_cont = getattr(self, "invCont", None)
                        if (
                            getattr(self, "destroyed", False)
                            or inv_cont is None
                            or getattr(inv_cont, "destroyed", False)
                            or getattr(sess, "mutating", False)
                            or _is_stale_inventory_controller_for_session(inv_cont, sess)
                        ):
                            return None
                        try:
                            return orig_fn(self, *args, **kwargs)
                        except Exception as exc:
                            if (
                                getattr(self, "destroyed", False)
                                or getattr(inv_cont, "destroyed", False)
                                or (
                                    _is_container_inaccessible_error(exc)
                                    and (
                                        _is_hangar_session_undocked()
                                        or _is_stale_inventory_controller_for_session(inv_cont, sess)
                                    )
                                )
                            ):
                                return None
                            raise

                    _wrapped_gauge_method._evomu_camera_guard = True
                    return _wrapped_gauge_method

                setattr(gauge_cls, meth_name, _make_gauge_guard(orig_meth))
        _HOOKED_CAMERA_GUARD_TARGETS.add("invContainer")
        changed = True

    inv_cache_mod = sys.modules.get("eve.client.script.environment.invCache")
    inv_container_cls = (
        getattr(inv_cache_mod, "invCacheContainer", None)
        if inv_cache_mod is not None
        else None
    )
    if (
        inv_container_cls is not None
        and "invCacheContainer" not in _HOOKED_CAMERA_GUARD_TARGETS
        and (isinstance(inv_container_cls, type) or hasattr(inv_container_cls, "__bases__"))
    ):
        orig_list = getattr(inv_container_cls, "List", None)
        if callable(orig_list) and not getattr(orig_list, "_evomu_camera_guard", False):
            orig_list_func = getattr(orig_list, "__func__", orig_list)

            def _wrapped_inv_cache_container_list(self, *args, **kwargs):
                if getattr(self, "moniker", None) is None:
                    return []
                try:
                    return orig_list_func(self, *args, **kwargs)
                except AttributeError:
                    if getattr(self, "moniker", None) is None:
                        return []
                    raise

            _wrapped_inv_cache_container_list.__name__ = getattr(
                orig_list_func, "__name__", "List"
            )
            _wrapped_inv_cache_container_list._evomu_camera_guard = True
            inv_container_cls.List = _wrapped_inv_cache_container_list
            _HOOKED_CAMERA_GUARD_TARGETS.add("invCacheContainer")
            changed = True
    return changed


_FITTING_INT_TYPES = (
    (int, getattr(__builtin__, "long")) if hasattr(__builtin__, "long") else (int,)
)


def _as_positive_ship_id(val):
    if isinstance(val, _FITTING_INT_TYPES) and not isinstance(val, bool) and val > 0:
        return val
    return None


def _is_simulated_fitting_controller(controller):
    if controller is None:
        return False
    is_sim = getattr(controller, "IsSimulated", None)
    if callable(is_sim):
        try:
            return bool(is_sim())
        except Exception:
            return False
    return False


def _resolve_fitting_dogma_location(controller=None):
    if controller is not None:
        get_dl = getattr(controller, "GetDogmaLocation", None)
        if callable(get_dl):
            try:
                dl = get_dl()
                if dl is not None:
                    return dl
            except Exception:
                pass
        dl = getattr(controller, "dogmaLocation", None)
        if dl is not None:
            return dl
    sm = getattr(__builtin__, "sm", None)
    if sm is not None and callable(getattr(sm, "IsServiceRunning", None)):
        try:
            if sm.IsServiceRunning("clientDogmaIM"):
                cdi = sm.GetService("clientDogmaIM")
                get_dl = getattr(cdi, "GetDogmaLocation", None)
                if callable(get_dl):
                    return get_dl()
        except Exception:
            pass
    return None


def _fitting_controller_ship_id(controller):
    if controller is None:
        return None
    get_id = getattr(controller, "GetItemID", None)
    if callable(get_id):
        try:
            ship_id = _as_positive_ship_id(get_id())
            if ship_id is not None:
                return ship_id
        except Exception:
            pass
    return _as_positive_ship_id(getattr(controller, "_itemID", None))


def _dogma_location_current_ship_id(dogma_loc):
    if dogma_loc is None:
        return None
    get_cur = getattr(dogma_loc, "GetCurrentShipID", None)
    if callable(get_cur):
        try:
            ship_id = _as_positive_ship_id(get_cur())
            if ship_id is not None:
                return ship_id
        except Exception:
            pass
    return _as_positive_ship_id(getattr(dogma_loc, "shipID", None))


def _snapshot_fitting_ship_ids(controller=None):
    ship_ids = set()
    if _is_simulated_fitting_controller(controller):
        return ship_ids
    dl = _resolve_fitting_dogma_location(controller)
    for candidate in (
        _fitting_controller_ship_id(controller),
        _dogma_location_current_ship_id(dl),
        _as_positive_ship_id(getattr(dl, "shipIDBeingDisembarked", None)) if dl is not None else None,
        _as_positive_ship_id(_session_ship_id()),
    ):
        if candidate is not None:
            ship_ids.add(candidate)
    return ship_ids


def _is_stale_ship_dogma_key_error(exc, controller=None, initial_ship_ids=None):
    """Return True iff *exc* is a KeyError for an unloaded ship ID during a live ship swap (#1410)."""
    if not isinstance(exc, KeyError) or not getattr(exc, "args", None):
        return False
    missing_key = _as_positive_ship_id(exc.args[0])
    if missing_key is None:
        return False
    if _is_simulated_fitting_controller(controller):
        return False

    dl = _resolve_fitting_dogma_location(controller)
    dogma_items = getattr(dl, "dogmaItems", None) if dl is not None else None
    if dl is None or dogma_items is None or not hasattr(dogma_items, "__contains__"):
        return False
    try:
        if missing_key in dogma_items:
            return False
    except Exception:
        return False

    ctrl_ship = _fitting_controller_ship_id(controller)
    dl_ship = _dogma_location_current_ship_id(dl)
    disembarking_ship = _as_positive_ship_id(getattr(dl, "shipIDBeingDisembarked", None))
    embarking_ship = _as_positive_ship_id(getattr(dl, "shipIDBeingEmbarked", None))
    sess = getattr(__builtin__, "session", None)
    sess_ship = _as_positive_ship_id(getattr(sess, "shipid", None)) if sess is not None else None

    known_ship_ids = set(initial_ship_ids) if initial_ship_ids else set()
    for cid in (ctrl_ship, disembarking_ship):
        if cid is not None:
            known_ship_ids.add(cid)

    if missing_key not in known_ship_ids:
        return False

    is_switching = False
    is_sw_fn = getattr(dl, "IsSwitchingShips", None)
    if callable(is_sw_fn):
        try:
            is_switching = bool(is_sw_fn())
        except Exception:
            is_switching = False
    if disembarking_ship is not None or embarking_ship is not None:
        is_switching = True

    if is_switching:
        return True
    if sess is not None and sess_ship != missing_key:
        return True
    if dl_ship is not None and dl_ship != missing_key:
        return True
    if ctrl_ship is not None and ctrl_ship != missing_key:
        return True
    return False


def _make_fitting_panel_stale_ship_guard(orig_fn, controller_attr="controller"):
    """Guard fitting panel stat refresh methods against unloaded stale ship KeyErrors (#1410)."""
    orig_func = getattr(orig_fn, "__func__", orig_fn)

    def _wrapped_fitting_panel_update(self, *args, **kwargs):
        if getattr(self, "destroyed", False):
            return None
        controller = getattr(self, controller_attr, None)
        initial_ship_ids = _snapshot_fitting_ship_ids(controller)
        try:
            return orig_func(self, *args, **kwargs)
        except KeyError as exc:
            live_controller = getattr(self, controller_attr, None) or controller
            if _is_stale_ship_dogma_key_error(
                exc,
                controller=live_controller,
                initial_ship_ids=initial_ship_ids,
            ):
                return None
            raise

    _wrapped_fitting_panel_update.__name__ = getattr(
        orig_func, "__name__", "_wrapped_fitting_panel_update"
    )
    _wrapped_fitting_panel_update._evomu_camera_guard = True
    return _wrapped_fitting_panel_update


def _install_fitting_active_ship_guard():
    """Guard fittingSvc/fittingWarnings/StatsPanel/DronePanel against dock & ship-swap races (#1254, #1410)."""
    changed = False
    fsvc_mod = sys.modules.get("eve.client.script.environment.fittingSvc")
    fsvc_cls = None
    if fsvc_mod is not None:
        fsvc_cls = getattr(fsvc_mod, "fittingSvc", None) or getattr(fsvc_mod, "FittingSvc", None)
    if (
        fsvc_cls is not None
        and "fittingSvc" not in _HOOKED_CAMERA_GUARD_TARGETS
        and (isinstance(fsvc_cls, type) or hasattr(fsvc_cls, "__bases__"))
    ):
        orig_get = getattr(fsvc_cls, "GetFittingDictForActiveShip", None)
        if callable(orig_get):
            if not getattr(orig_get, "_evomu_camera_guard", False):
                def _wrapped_get_fitting_dict_for_active_ship(self, *args, **kwargs):
                    eve_cfg = getattr(fsvc_mod, "eveCfg", None)
                    active_ship = None
                    if eve_cfg is not None and callable(getattr(eve_cfg, "GetActiveShip", None)):
                        active_ship = eve_cfg.GetActiveShip()
                    else:
                        sess = getattr(__builtin__, "session", None)
                        active_ship = getattr(sess, "shipid", None)
                    if active_ship is None:
                        raise RuntimeError("No active ship")
                    return orig_get(self, *args, **kwargs)

                _wrapped_get_fitting_dict_for_active_ship._evomu_camera_guard = True
                fsvc_cls.GetFittingDictForActiveShip = _wrapped_get_fitting_dict_for_active_ship
                changed = True
            _HOOKED_CAMERA_GUARD_TARGETS.add("fittingSvc")

    fw_mod = sys.modules.get("shipfitting.fittingWarnings")
    if fw_mod is not None and "fittingWarnings" not in _HOOKED_CAMERA_GUARD_TARGETS:
        orig_eval = getattr(fw_mod, "EvaluateFit", None)
        if callable(orig_eval):
            if not getattr(orig_eval, "_evomu_camera_guard", False):
                def _wrapped_evaluate_fit(fittingController):
                    initial_ship_ids = _snapshot_fitting_ship_ids(fittingController)
                    try:
                        return orig_eval(fittingController)
                    except IndexError as exc:
                        sess = getattr(__builtin__, "session", None)
                        if getattr(sess, "shipid", None) is None or "None" in str(exc):
                            return None
                        raise
                    except KeyError as exc:
                        if _is_stale_ship_dogma_key_error(
                            exc,
                            controller=fittingController,
                            initial_ship_ids=initial_ship_ids,
                        ):
                            return None
                        raise

                _wrapped_evaluate_fit._evomu_camera_guard = True
                fw_mod.EvaluateFit = _wrapped_evaluate_fit
                changed = True
            _HOOKED_CAMERA_GUARD_TARGETS.add("fittingWarnings")

    wic_mod = sys.modules.get("eve.client.script.ui.shared.fittingScreen.warningIconCont")
    wic_cls = getattr(wic_mod, "WarningIconCont", None) if wic_mod is not None else None
    if (
        wic_cls is not None
        and "warningIconCont" not in _HOOKED_CAMERA_GUARD_TARGETS
        and (isinstance(wic_cls, type) or hasattr(wic_cls, "__bases__"))
    ):
        orig_thread = getattr(wic_cls, "_WarningIconCont__UpdateIcon_thread", None)
        if callable(orig_thread):
            if not getattr(orig_thread, "_evomu_camera_guard", False):
                orig_thread_func = getattr(orig_thread, "__func__", orig_thread)

                def _wrapped_update_icon_thread(self):
                    if getattr(self, "destroyed", False):
                        return None
                    controller = getattr(self, "fittingController", None)
                    initial_ship_ids = _snapshot_fitting_ship_ids(controller)
                    try:
                        return orig_thread_func(self)
                    except IndexError as exc:
                        sess = getattr(__builtin__, "session", None)
                        if getattr(sess, "shipid", None) is None or "None" in str(exc):
                            return None
                        raise
                    except KeyError as exc:
                        live_controller = getattr(self, "fittingController", None) or controller
                        if _is_stale_ship_dogma_key_error(
                            exc,
                            controller=live_controller,
                            initial_ship_ids=initial_ship_ids,
                        ):
                            return None
                        raise

                _wrapped_update_icon_thread._evomu_camera_guard = True
                wic_cls._WarningIconCont__UpdateIcon_thread = _wrapped_update_icon_thread
                changed = True
            _HOOKED_CAMERA_GUARD_TARGETS.add("warningIconCont")

    if _wrap_cls_methods_table(
        "statsPanel",
        "eve.client.script.ui.shared.fittingScreen.statsPanel",
        "StatsPanel",
        (
            ("UpdateStats", _make_fitting_panel_stale_ship_guard),
            ("UpdateDroneStats", _make_fitting_panel_stale_ship_guard),
        ),
    ):
        changed = True

    if _wrap_cls_methods_table(
        "dronePanel",
        "eve.client.script.ui.shared.fittingScreen.fittingPanels.dronePanel",
        "DronePanel",
        (("UpdateDroneStats", _make_fitting_panel_stale_ship_guard),),
    ):
        changed = True

    return changed


def _make_modular_skin_change_guard(orig_fn):
    """Skip ProcessSkinChange when playerShipItemID is unset or stale vs session.shipid (#1286)."""
    def _wrapped_process_skin_change(self, newModel, *args, **kwargs):
        player_ship_id = getattr(self, "playerShipItemID", None)
        if not player_ship_id:
            return None
        sess_ship = _session_ship_id()
        if sess_ship is not None and player_ship_id != sess_ship:
            return None
        return orig_fn(self, newModel, *args, **kwargs)

    _wrapped_process_skin_change._evomu_camera_guard = True
    return _wrapped_process_skin_change


def _make_hangar_ship_damage_guard(orig_fn):
    """Skip BaseHangarShipBehaviour.SetShipDamage for unset, stale, or undocked ships (#1286)."""
    def _wrapped_set_ship_damage(self, itemID, model, *args, **kwargs):
        if not itemID or _is_hangar_session_undocked():
            return False
        sess_ship = _session_ship_id()
        if sess_ship is not None and itemID != sess_ship:
            return False
        return orig_fn(self, itemID, model, *args, **kwargs)

    _wrapped_set_ship_damage._evomu_camera_guard = True
    return _wrapped_set_ship_damage


def _wrap_cls_method_if_present(cls, method_name, wrapper_factory):
    orig_fn = getattr(cls, method_name, None)
    if callable(orig_fn) and not getattr(orig_fn, "_evomu_camera_guard", False):
        setattr(cls, method_name, wrapper_factory(orig_fn))
        return True
    return False


def _wrap_cls_methods_table(target_key, mod_name, cls_name, method_specs):
    """Wrap each listed ``method_name`` on ``mod.cls`` once per session."""
    if target_key in _HOOKED_CAMERA_GUARD_TARGETS:
        return False
    mod = sys.modules.get(mod_name)
    cls = getattr(mod, cls_name, None) if mod is not None else None
    if cls is None or not (isinstance(cls, type) or hasattr(cls, "__bases__")):
        return False
    hooked = False
    for method_name, wrapper_factory in method_specs:
        if _wrap_cls_method_if_present(cls, method_name, wrapper_factory):
            hooked = True
    if hooked:
        _HOOKED_CAMERA_GUARD_TARGETS.add(target_key)
    return hooked


def _install_cls_method_guard(target_key, mod_name, cls_name, method_name, wrapper_factory):
    """Wrap ``mod.cls.method_name`` once per session using ``wrapper_factory``."""
    return _wrap_cls_methods_table(target_key, mod_name, cls_name, ((method_name, wrapper_factory),))


def _install_base_hangar_behaviour_guard():
    """Guard BaseHangarShipBehaviour.SetShipDamage against stale unboarded ship IDs (#1286)."""
    return _install_cls_method_guard(
        "baseHangarBehaviours",
        "eve.client.script.ui.view.hangarBehaviours.baseHangarBehaviours",
        "BaseHangarShipBehaviour",
        "SetShipDamage",
        _make_hangar_ship_damage_guard,
    )


def _install_modular_capital_hangar_guard():
    """Guard ModularCapitalHangarShipBehaviour.ProcessSkinChange against stale ship IDs (#1286)."""
    return _install_cls_method_guard(
        "modularHangarCapitalBehaviours",
        "eve.client.script.ui.view.hangarBehaviours.modularHangarCapitalBehaviours",
        "ModularCapitalHangarShipBehaviour",
        "ProcessSkinChange",
        _make_modular_skin_change_guard,
    )


def _install_modular_hangar_teardown_guard():
    """Guard ModularHangarShipBehaviour against CleanScene None-camera (#1218) and stale ProcessSkinChange (#1286)."""
    if "modularHangarBehaviours" in _HOOKED_CAMERA_GUARD_TARGETS:
        return False
    mhb_mod = sys.modules.get("eve.client.script.ui.view.hangarBehaviours.modularHangarBehaviours")
    if mhb_mod is None:
        return False
    hooked = False
    for cls_name in ("ModularHangarShipBehaviour", "ModularHangarDroneBehaviour"):
        cls = getattr(mhb_mod, cls_name, None)
        if cls is None or not (isinstance(cls, type) or hasattr(cls, "__bases__")):
            continue
        orig_clean = getattr(cls, "CleanScene", None)
        if callable(orig_clean) and not getattr(orig_clean, "_evomu_camera_guard", False):
            def _make_clean_guard(orig_fn):
                def _wrapped_clean_scene(self):
                    if getattr(self, "smallDocks", None) is None:
                        self.smallDocks = []
                    if getattr(self, "bigDocks", None) is None:
                        self.bigDocks = []
                    override_cam, cam_val = _begin_null_camera_override(self)
                    try:
                        return orig_fn(self)
                    except AttributeError as exc:
                        if _is_none_clear_refs_error(exc) or ("ClearRefs" in str(exc) and cam_val is None):
                            return None
                        raise
                    finally:
                        _end_null_camera_override(self, override_cam)

                _wrapped_clean_scene._evomu_camera_guard = True
                return _wrapped_clean_scene

            cls.CleanScene = _make_clean_guard(orig_clean)
            hooked = True
        if cls_name == "ModularHangarShipBehaviour":
            if _wrap_cls_method_if_present(cls, "ProcessSkinChange", _make_modular_skin_change_guard):
                hooked = True
    if hooked:
        _HOOKED_CAMERA_GUARD_TARGETS.add("modularHangarBehaviours")
    return hooked


def _make_wreck_none_model_guard(orig_fn):
    """Guard Wreck Prepare/rotation methods when async model loading or Release() leaves self.model None (#1217)."""
    def _wrapped_wreck_method(self, *args, **kwargs):
        try:
            return orig_fn(self, *args, **kwargs)
        except AttributeError:
            if getattr(self, "model", None) is None:
                return None
            raise

    _wrapped_wreck_method._evomu_camera_guard = True
    return _wrapped_wreck_method


def _make_space_mgr_wreck_guard(orig_fn):
    """Install Wreck guards before SpaceMgr adds/prepares lazily imported spaceObject.Wreck instances (#1217)."""
    def _wrapped_space_mgr_call(self, *args, **kwargs):
        if "wreck" not in _HOOKED_CAMERA_GUARD_TARGETS:
            _install_wreck_rotation_guard()
        return orig_fn(self, *args, **kwargs)

    _wrapped_space_mgr_call._evomu_camera_guard = True
    return _wrapped_space_mgr_call


def _install_wreck_rotation_guard():
    """Guard Wreck.Prepare/rotation methods when self.model is None on persisted or unloaded wrecks (#1217)."""
    hooked_wreck = _wrap_cls_methods_table(
        "wreck",
        "eve.client.script.environment.spaceObject.wreck",
        "Wreck",
        (
            ("Prepare", _make_wreck_none_model_guard),
            ("SetRandomRotation", _make_wreck_none_model_guard),
            ("SetDungeonRotation", _make_wreck_none_model_guard),
            ("SetBallRotation", _make_wreck_none_model_guard),
        ),
    )
    hooked_space_mgr = _wrap_cls_methods_table(
        "spaceMgrWreck",
        "eve.client.script.parklife.spaceMgr",
        "SpaceMgr",
        (
            ("DoBallsAdded_", _make_space_mgr_wreck_guard),
            ("_LoadObjects", _make_space_mgr_wreck_guard),
            ("LoadObject", _make_space_mgr_wreck_guard),
        ),
    )
    return hooked_wreck or hooked_space_mgr


def _install_slots_container_sync_guard():
    """Ensure SlotsContainer rebuilds HUD slots on OnModuleFitted / late ProcessShipEffect (#1252)."""
    if "slotsContainer" in _HOOKED_CAMERA_GUARD_TARGETS:
        return False
    sc_mod = sys.modules.get("eve.client.script.ui.inflight.shipHud.slotsContainer")
    sc_cls = getattr(sc_mod, "SlotsContainer", None) if sc_mod is not None else None
    if sc_cls is None or not (isinstance(sc_cls, type) or hasattr(sc_cls, "__bases__")):
        return False

    def _schedule_slots_refresh(cont):
        if getattr(cont, "destroyed", False):
            return
        init_delayed = getattr(cont, "InitSlotsDelayed", None)
        if callable(init_delayed):
            init_delayed()

    notify_events = getattr(sc_cls, "__notifyevents__", None)
    if isinstance(notify_events, (list, tuple)) and "OnModuleFitted" not in notify_events:
        if isinstance(notify_events, list):
            notify_events.append("OnModuleFitted")
        else:
            sc_cls.__notifyevents__ = list(notify_events) + ["OnModuleFitted"]

    if not getattr(getattr(sc_cls, "OnModuleFitted", None), "_evomu_camera_guard", False):
        def _on_module_fitted(self, *args, **kwargs):
            _schedule_slots_refresh(self)

        _on_module_fitted._evomu_camera_guard = True
        sc_cls.OnModuleFitted = _on_module_fitted

    orig_pse = getattr(sc_cls, "ProcessShipEffect", None)
    if callable(orig_pse) and not getattr(orig_pse, "_evomu_camera_guard", False):
        def _wrapped_process_ship_effect(self, godmaStm, effectState):
            res = orig_pse(self, godmaStm, effectState)
            if not getattr(self, "destroyed", False) and effectState is not None:
                item_id = getattr(effectState, "itemID", None)
                mods_by_id = getattr(self, "modulesByID", None) or {}
                if item_id is not None and item_id not in mods_by_id:
                    ctrl = getattr(self, "controller", None)
                    get_mods = getattr(ctrl, "GetModules", None) if ctrl is not None else None
                    if callable(get_mods):
                        for m in get_mods() or ():
                            if getattr(m, "itemID", None) == item_id:
                                _schedule_slots_refresh(self)
                                break
            return res

        _wrapped_process_ship_effect._evomu_camera_guard = True
        sc_cls.ProcessShipEffect = _wrapped_process_ship_effect

    orig_inv = getattr(sc_cls, "OnInvChange", None)
    if callable(orig_inv) and not getattr(orig_inv, "_evomu_camera_guard", False):
        def _wrapped_on_inv_change(self, item, change):
            res = orig_inv(self, item, change)
            if not getattr(self, "destroyed", False) and item is not None:
                sess = getattr(__builtin__, "session", None)
                ship_id = getattr(sess, "shipid", None)
                item_id = getattr(item, "itemID", None)
                mods_by_id = getattr(self, "modulesByID", None) or {}
                if (
                    ship_id is not None
                    and getattr(item, "locationID", None) == ship_id
                    and item_id is not None
                    and item_id not in mods_by_id
                ):
                    is_fit_flag = getattr(sc_mod, "IsShipFittingFlag", None)
                    if callable(is_fit_flag) and is_fit_flag(getattr(item, "flagID", None)):
                        _schedule_slots_refresh(self)
            return res

        _wrapped_on_inv_change._evomu_camera_guard = True
        sc_cls.OnInvChange = _wrapped_on_inv_change

    _HOOKED_CAMERA_GUARD_TARGETS.add("slotsContainer")
    return True


def _is_sensor_gfx_load_error(exc):
    return "Couldn't get object" in str(exc)


def _can_run_sensor_gfx_swipe(gfx_handler):
    sess = getattr(__builtin__, "session", None)
    if sess is not None and (
        getattr(sess, "solarsystemid", None) is None
        or getattr(sess, "stationid", None) is not None
        or getattr(sess, "structureid", None) is not None
        or getattr(sess, "mutating", False)
    ):
        return False
    sm = getattr(__builtin__, "sm", None)
    if sm is not None and callable(getattr(sm, "IsServiceRunning", None)):
        try:
            if sm.IsServiceRunning("subway"):
                subway = sm.GetService("subway")
                in_jump = getattr(subway, "InJump", None)
                if callable(in_jump) and in_jump():
                    return False
        except Exception:
            pass
    if hasattr(gfx_handler, "sceneManager"):
        scene_mgr = getattr(gfx_handler, "sceneManager", None)
        if scene_mgr is None:
            return False
        get_scene = getattr(scene_mgr, "GetRegisteredScene", None)
        if callable(get_scene) and get_scene("default") is None:
            return False
    return True


def _make_sensor_start_gfx_swipe_guard(orig_fn):
    def _wrapped_start_gfx_swipe(self, *args, **kwargs):
        if not _can_run_sensor_gfx_swipe(self):
            return None
        try:
            return orig_fn(self, *args, **kwargs)
        except RuntimeError as exc:
            if _is_sensor_gfx_load_error(exc) and getattr(self, "gfxSensorSwipe", None) is None:
                return None
            raise

    _wrapped_start_gfx_swipe._evomu_camera_guard = True
    return _wrapped_start_gfx_swipe


def _make_sensor_gfx_swipe_thread_guard(orig_fn):
    def _wrapped_gfx_swipe_thread(self, *args, **kwargs):
        try:
            return orig_fn(self, *args, **kwargs)
        except RuntimeError as exc:
            if _is_sensor_gfx_load_error(exc) and getattr(self, "gfxSensorSwipe", None) is None:
                return None
            raise
        finally:
            if hasattr(self, "gfxSwipeThread"):
                self.gfxSwipeThread = None

    _wrapped_gfx_swipe_thread._evomu_camera_guard = True
    return _wrapped_gfx_swipe_thread


def _install_sensor_suite_gfx_guard():
    """Guard sensorsuite.overlay.gfxhandler.GfxHandler against jump/scene-reload races (#1292, #1293)."""
    if "sensorGfxHandler" in _HOOKED_CAMERA_GUARD_TARGETS:
        return False
    gfx_mod = sys.modules.get("sensorsuite.overlay.gfxhandler")
    gfx_cls = getattr(gfx_mod, "GfxHandler", None) if gfx_mod is not None else None
    if gfx_cls is None or not (isinstance(gfx_cls, type) or hasattr(gfx_cls, "__bases__")):
        return False
    hooked = False
    if _wrap_cls_method_if_present(gfx_cls, "StartGfxSwipe", _make_sensor_start_gfx_swipe_guard):
        hooked = True
    if _wrap_cls_method_if_present(gfx_cls, "_GfxSwipeThread", _make_sensor_gfx_swipe_thread_guard):
        hooked = True
    if hooked:
        _HOOKED_CAMERA_GUARD_TARGETS.add("sensorGfxHandler")
    return hooked


def _noop_camera_method(self, *args, **kwargs):
    return None


def _make_none_model_guard(orig_fn, fallback_fn):
    """Return ``fallback_fn`` when ``self.model is None``, otherwise run ``orig_fn`` unmasked (#1318)."""
    def _wrapped_none_model_method(self, *args, **kwargs):
        if getattr(self, "model", None) is None:
            return fallback_fn(self, *args, **kwargs)
        return orig_fn(self, *args, **kwargs)

    _wrapped_none_model_method._evomu_camera_guard = True
    return _wrapped_none_model_method


def _safe_modular_zoom_point(cam):
    get_zoom_pt = getattr(cam, "GetZoomToPoint", None)
    if callable(get_zoom_pt):
        try:
            zoom_pt = get_zoom_pt()
            if _is_finite_safe_vec3(zoom_pt):
                return _vec3_tuple(zoom_pt, (0.0, 0.0, 0.0))
        except Exception:
            pass
    at_pos = getattr(cam, "_atPosition", getattr(cam, "atPosition", None))
    return _vec3_tuple(at_pos, (0.0, 0.0, 0.0)) if _is_finite_safe_vec3(at_pos) else (0.0, 0.0, 0.0)


def _safe_modular_target_point(cam, *args, **kwargs):
    zoom_pt = _safe_modular_zoom_point(cam)
    eye_pos = None
    get_eye_pos = getattr(cam, "GetEyePosition", None)
    if callable(get_eye_pos):
        try:
            eye_pos = get_eye_pos()
        except Exception:
            eye_pos = None
    if eye_pos is None:
        eye_pos = getattr(cam, "_eyePosition", None)
    if _is_finite_safe_vec3(eye_pos):
        eye_tuple = _vec3_tuple(eye_pos, (0.0, 500.0, 1000.0))
        dist_sq = (
            (eye_tuple[0] - zoom_pt[0]) ** 2
            + (eye_tuple[1] - zoom_pt[1]) ** 2
            + (eye_tuple[2] - zoom_pt[2]) ** 2
        )
        if dist_sq > 1e-3:
            return eye_tuple
    return (zoom_pt[0], zoom_pt[1] + 500.0, zoom_pt[2] + 1000.0)


def _safe_modular_zoom_constants(cam, *args, **kwargs):
    if getattr(cam, "currentShipSizeClass", 0) == 2:
        return (1450.0, 250.0, 500.0)
    return (385.0, 10.0, 120.0)


def _make_modular_anim_enter_guard(orig_fn):
    """Handle ModularHangarCamera.AnimEnterHangar(None, ...) without crashing or scheduling None AllignCamera (#1318)."""
    def _wrapped_anim_enter_hangar(self, *args, **kwargs):
        model = args[0] if args else kwargs.get("model")
        if model is None:
            gametime_mod = sys.modules.get("gametime")
            get_sim_time = getattr(gametime_mod, "GetSimTime", None) if gametime_mod is not None else None
            if callable(get_sim_time):
                self.hangarEntryTime = get_sim_time()
            self.model = None
            calc_focal = getattr(self, "_CalculateFocalPoint", None)
            self.atPosition = calc_focal(None) if callable(calc_focal) else (0.0, 0.0, 0.0)
            self.changingShip = False
            target = (
                self.CalculateTargetPoint()
                if callable(getattr(self, "CalculateTargetPoint", None))
                else _safe_modular_target_point(self)
            )
            set_eye = getattr(self, "SetEyePosition", None)
            if callable(set_eye):
                set_eye(target)
            self.yaw = 0
            self.pitch = 0
            return None
        return orig_fn(self, *args, **kwargs)

    _wrapped_anim_enter_hangar._evomu_camera_guard = True
    return _wrapped_anim_enter_hangar


def _safe_capital_orbit_target(cam, *args, **kwargs):
    orbit_target = getattr(cam, "orbitTarget", None)
    if _is_finite_safe_vec3(orbit_target):
        return _vec3_tuple(orbit_target, (3150.0, 2220.0, 16500.0))
    return (3150.0, 2220.0, 16500.0)


def _make_capital_anim_enter_guard(orig_fn):
    """Guard ModularHangarCapitalCamera.AnimEnterHangar against None ship models (#1318)."""
    def _wrapped_capital_anim_enter(self, *args, **kwargs):
        model = args[0] if args else kwargs.get("model")
        if model is None or getattr(self, "model", None) is None:
            target = _safe_capital_orbit_target(self)
            self.orbitTarget = target
            set_eye = getattr(self, "SetEyePosition", None)
            if callable(set_eye):
                set_eye(target)
            return None
        return orig_fn(self, *args, **kwargs)

    _wrapped_capital_anim_enter._evomu_camera_guard = True
    return _wrapped_capital_anim_enter


def _make_capital_start_locations_guard(orig_fn):
    """Downgrade wasOrbiting when model is None so _configureCameraStartLocations skips modelWorldPosition (#1318)."""
    def _wrapped_configure_start_locations(self, typeID, wasOrbiting=False, *args, **kwargs):
        if getattr(self, "model", None) is None and (wasOrbiting or kwargs.get("wasOrbiting")):
            return orig_fn(self, typeID, False)
        return orig_fn(self, typeID, wasOrbiting, *args, **kwargs)

    _wrapped_configure_start_locations._evomu_camera_guard = True
    return _wrapped_configure_start_locations


def _make_capital_orbit_thread_guard(orig_fn):
    """Exit CapitalOrbitUpdateThread cleanly if self.model is None while not lerping (#1318)."""
    def _wrapped_capital_orbit_thread(self, *args, **kwargs):
        if getattr(self, "model", None) is None and not getattr(self, "lerpingIntoPosition", True):
            self.orbitUpdateThread = None
            return None
        try:
            return orig_fn(self, *args, **kwargs)
        except AttributeError:
            if getattr(self, "model", None) is None:
                self.orbitUpdateThread = None
                return None
            raise

    _wrapped_capital_orbit_thread._evomu_camera_guard = True
    return _wrapped_capital_orbit_thread


def _install_modular_hangar_camera_guard():
    """Guard ModularHangarCamera and ModularHangarCapitalCamera against None ship models (#1318)."""
    hooked_std = _wrap_cls_methods_table(
        "modularHangarCamera",
        "eve.client.script.ui.camera.modularHangarCamera",
        "ModularHangarCamera",
        (
            ("AnimEnterHangar", _make_modular_anim_enter_guard),
            ("CalculateTargetPoint", lambda fn: _make_none_model_guard(fn, _safe_modular_target_point)),
            ("GetSizeDependantZoomConstants", lambda fn: _make_none_model_guard(fn, _safe_modular_zoom_constants)),
            ("_updateShipChangeLocations", lambda fn: _make_none_model_guard(fn, _noop_camera_method)),
        ),
    )
    hooked_cap = _wrap_cls_methods_table(
        "modularHangarCapitalCamera",
        "eve.client.script.ui.camera.modularHangarCapitalCamera",
        "ModularHangarCapitalCamera",
        (
            ("AnimEnterHangar", _make_capital_anim_enter_guard),
            ("CalculateOrbitTarget", lambda fn: _make_none_model_guard(fn, _safe_capital_orbit_target)),
            ("Orbit", lambda fn: _make_none_model_guard(fn, _noop_camera_method)),
            ("_configureCameraStartLocations", _make_capital_start_locations_guard),
            ("CapitalOrbitUpdateThread", _make_capital_orbit_thread_guard),
        ),
    )
    return hooked_std or hooked_cap


_SKILL_PLAN_PANEL_SIGNAL_SPECS = (
    (
        "skillPlanSignals",
        "skills.skillplan.skillPlanSignals",
        "skills.skillplan",
        (
            ("on_created", "OnSkillPlanCreated"),
            ("on_saved", "OnSkillPlanSaved"),
            ("on_deleted", "OnSkillPlanDeleted"),
        ),
    ),
    (
        "skillPlanUISignals",
        "eve.client.script.ui.skillPlan.skillPlanUISignals",
        "eve.client.script.ui.skillPlan",
        (
            ("on_create_new_personal_button", "OnCreateNewPersonalSkillPlanButton"),
            ("on_create_new_corp_button", "OnCreateNewCorpSkillPlanButton"),
            ("on_edit_button", "OnEditSkillPlanButton"),
            ("on_delete_button", "OnDeleteSkillPlanButton"),
            ("on_close_new_button", "OnCloseNewSkillPlanButton"),
            ("on_selected", "OnSkillPlanSelected"),
            ("on_close_button", "OnSkillPlanCloseButton"),
            ("on_show_multi_training_msg", "OnShowMultiTrainingMessage"),
            ("on_show_alpha_training_msg", "OnShowAlphaTrainingMessage"),
        ),
    ),
)


def _iter_skill_plan_signal_containers(panel_mod, attr_name, mod_name, pkg_name):
    """Return unique skill-plan signal container modules/objects in lookup order (#1332)."""
    containers = []
    for candidate in (
        getattr(panel_mod, attr_name, None) if panel_mod is not None else None,
        sys.modules.get(mod_name),
        getattr(sys.modules.get(pkg_name), attr_name, None) if pkg_name else None,
    ):
        if candidate is not None and not any(c is candidate for c in containers):
            containers.append(candidate)
    return containers


def _iter_skill_plan_panel_signals(panel_mod):
    """Yield ``(signal_obj, method_name)`` pairs across skill-plan signal containers (#1332)."""
    for attr_name, mod_name, pkg_name, slot_pairs in _SKILL_PLAN_PANEL_SIGNAL_SPECS:
        for container in _iter_skill_plan_signal_containers(panel_mod, attr_name, mod_name, pkg_name):
            for sig_name, method_name in slot_pairs:
                sig = getattr(container, sig_name, None)
                if sig is not None:
                    yield sig, method_name


def _prune_empty_signal_methods(methods_map, obj, funcs):
    """Remove empty WeakSet entries from ``Signal._methods`` so closed panels drop out immediately (#1332)."""
    if hasattr(funcs, "__len__") and len(funcs) == 0 and hasattr(methods_map, "pop"):
        try:
            methods_map.pop(obj, None)
        except Exception:
            pass


def _discard_panel_slot_from_signal(sig, panel, slot, orig_func=None):
    """Idempotently disconnect *panel*'s slot (and any pre-wrap function) from *sig* (#1332)."""
    disconnect_fn = getattr(sig, "disconnect", None)
    if slot is not None and callable(disconnect_fn):
        try:
            disconnect_fn(slot)
        except Exception:
            pass
    methods_map = getattr(sig, "_methods", None)
    if methods_map is None or not hasattr(methods_map, "get"):
        return
    try:
        funcs = methods_map.get(panel)
    except Exception:
        return
    if funcs is None or not hasattr(funcs, "discard"):
        return
    slot_func = getattr(slot, "__func__", None) if slot is not None else None
    if slot_func is not None:
        funcs.discard(slot_func)
    if orig_func is not None:
        funcs.discard(orig_func)
    _prune_empty_signal_methods(methods_map, panel, funcs)


def _disconnect_skill_plan_panel_signals(panel, panel_mod=None, orig_slot_funcs=None):
    """Disconnect all 12 SkillPlanPanel bound signal slots on panel teardown (#1332)."""
    if panel is None:
        return
    live_panel_mod = sys.modules.get("eve.client.script.ui.skillPlan.skillPlanPanel") or panel_mod
    for sig, method_name in _iter_skill_plan_panel_signals(live_panel_mod):
        slot = getattr(panel, method_name, None)
        orig_func = orig_slot_funcs.get(method_name) if orig_slot_funcs else None
        _discard_panel_slot_from_signal(sig, panel, slot, orig_func=orig_func)


def _make_skill_plan_panel_slot_guard(orig_fn):
    """Guard a SkillPlanPanel global signal slot so destroyed instances no-op (#1332)."""
    def _wrapped_skill_plan_panel_slot(self, *args, **kwargs):
        if getattr(self, "destroyed", False):
            return None
        return orig_fn(self, *args, **kwargs)

    _wrapped_skill_plan_panel_slot.__name__ = getattr(
        orig_fn, "__name__", "_wrapped_skill_plan_panel_slot"
    )
    _wrapped_skill_plan_panel_slot._evomu_camera_guard = True
    return _wrapped_skill_plan_panel_slot


def _migrate_skill_plan_signal_slots(panel_mod, wrapped_slots):
    """Migrate pre-existing Signal._methods slots to guarded wrappers and purge destroyed panels (#1332)."""
    if not wrapped_slots:
        return
    for sig, method_name in _iter_skill_plan_panel_signals(panel_mod):
        pair = wrapped_slots.get(method_name)
        if pair is None:
            continue
        orig_func, new_func = pair
        methods_map = getattr(sig, "_methods", None)
        if methods_map is None or not hasattr(methods_map, "items"):
            continue
        try:
            entries = list(methods_map.items())
        except Exception:
            continue
        for obj, funcs in entries:
            if funcs is None or not hasattr(funcs, "discard"):
                continue
            try:
                has_orig = orig_func in funcs
            except Exception:
                has_orig = False
            if has_orig:
                funcs.discard(orig_func)
                if not getattr(obj, "destroyed", False) and hasattr(funcs, "add"):
                    funcs.add(new_func)
            _prune_empty_signal_methods(methods_map, obj, funcs)


def _install_skill_plan_panel_signal_guard():
    """Disconnect SkillPlanPanel global signals on Close and guard slots when destroyed (#1332)."""
    if "skillPlanPanel" in _HOOKED_CAMERA_GUARD_TARGETS:
        return False
    panel_mod = sys.modules.get("eve.client.script.ui.skillPlan.skillPlanPanel")
    panel_cls = getattr(panel_mod, "SkillPlanPanel", None) if panel_mod is not None else None
    if panel_cls is None or not (isinstance(panel_cls, type) or hasattr(panel_cls, "__bases__")):
        return False

    hooked = False
    orig_slot_funcs = {}
    wrapped_slots = {}
    for _attr_name, _mod_name, _pkg_name, slot_pairs in _SKILL_PLAN_PANEL_SIGNAL_SPECS:
        for _sig_name, method_name in slot_pairs:
            orig_attr = getattr(panel_cls, method_name, None)
            if callable(orig_attr) and not getattr(orig_attr, "_evomu_camera_guard", False):
                orig_func = getattr(orig_attr, "__func__", orig_attr)
                wrapped = _make_skill_plan_panel_slot_guard(orig_func)
                setattr(panel_cls, method_name, wrapped)
                new_attr = getattr(panel_cls, method_name, wrapped)
                new_func = getattr(new_attr, "__func__", wrapped)
                orig_slot_funcs[method_name] = orig_func
                wrapped_slots[method_name] = (orig_func, new_func)
                hooked = True

    _migrate_skill_plan_signal_slots(panel_mod, wrapped_slots)

    orig_close = getattr(panel_cls, "Close", None)
    if callable(orig_close) and not getattr(orig_close, "_evomu_camera_guard", False):
        orig_close_func = getattr(orig_close, "__func__", orig_close)

        def _wrapped_skill_plan_panel_close(self, *args, **kwargs):
            try:
                return orig_close_func(self, *args, **kwargs)
            finally:
                _disconnect_skill_plan_panel_signals(
                    self,
                    panel_mod=panel_mod,
                    orig_slot_funcs=orig_slot_funcs,
                )

        _wrapped_skill_plan_panel_close.__name__ = getattr(orig_close_func, "__name__", "Close")
        _wrapped_skill_plan_panel_close._evomu_camera_guard = True
        panel_cls.Close = _wrapped_skill_plan_panel_close
        hooked = True

    if hooked:
        _HOOKED_CAMERA_GUARD_TARGETS.add("skillPlanPanel")
    return hooked


_PROBE_DATA_MISSING = object()


def _extract_probe_id(item):
    probe_id = getattr(item, "probeID", None)
    if probe_id is not None:
        return probe_id
    if isinstance(item, dict):
        return item.get("probeID")
    return None


def _is_unknown_probe_id(tracker, probe_id):
    if probe_id is None:
        return False
    probe_data = getattr(tracker, "probeData", _PROBE_DATA_MISSING)
    if probe_data is _PROBE_DATA_MISSING:
        return False
    if probe_data is None:
        return True
    try:
        return probe_id not in probe_data
    except Exception:
        return False


def _install_probe_tracker_guard():
    """Guard ProbeTracker against unknown probeIDs on jump/reconnect notification races (#1334)."""
    if "probeTracker" in _HOOKED_CAMERA_GUARD_TARGETS:
        return False
    pt_mod = sys.modules.get("probescanning.probeTracker")
    pt_cls = getattr(pt_mod, "ProbeTracker", None) if pt_mod is not None else None
    if pt_cls is None or not (isinstance(pt_cls, type) or hasattr(pt_cls, "__bases__")):
        return False

    hooked = False

    orig_on_idle = getattr(pt_cls, "OnProbesIdle", None)
    if callable(orig_on_idle) and not getattr(orig_on_idle, "_evomu_camera_guard", False):
        orig_on_idle_func = getattr(orig_on_idle, "__func__", orig_on_idle)

        def _wrapped_on_probes_idle(self, probes, *args, **kwargs):
            probe_data = getattr(self, "probeData", _PROBE_DATA_MISSING)
            filtered = probes
            input_probe_ids = set()
            if probes is not None:
                try:
                    seq = list(probes)
                except Exception:
                    seq = None
                if seq is not None:
                    kept = []
                    for item in seq:
                        pid = _extract_probe_id(item)
                        if pid is not None:
                            input_probe_ids.add(pid)
                        if probe_data is _PROBE_DATA_MISSING or pid is None or not _is_unknown_probe_id(self, pid):
                            kept.append(item)
                    if probe_data is not _PROBE_DATA_MISSING:
                        filtered = tuple(kept) if isinstance(probes, tuple) else kept
            try:
                return orig_on_idle_func(self, filtered, *args, **kwargs)
            except KeyError as exc:
                if (
                    exc.args
                    and exc.args[0] in input_probe_ids
                    and _is_unknown_probe_id(self, exc.args[0])
                ):
                    return None
                raise

        _wrapped_on_probes_idle.__name__ = getattr(orig_on_idle_func, "__name__", "OnProbesIdle")
        _wrapped_on_probes_idle._evomu_camera_guard = True
        pt_cls.OnProbesIdle = _wrapped_on_probes_idle
        hooked = True

    for meth_name in ("SetProbeDestination", "UpdateProbeState"):
        orig_meth = getattr(pt_cls, meth_name, None)
        if callable(orig_meth) and not getattr(orig_meth, "_evomu_camera_guard", False):
            orig_meth_func = getattr(orig_meth, "__func__", orig_meth)

            def _make_probe_id_guard(orig_fn, name):
                def _wrapped_probe_method(self, *args, **kwargs):
                    probe_id = args[0] if args else kwargs.get("probeID")
                    if _is_unknown_probe_id(self, probe_id):
                        return None
                    try:
                        return orig_fn(self, *args, **kwargs)
                    except KeyError as exc:
                        if (
                            exc.args
                            and exc.args[0] == probe_id
                            and _is_unknown_probe_id(self, exc.args[0])
                        ):
                            return None
                        raise

                _wrapped_probe_method.__name__ = name
                _wrapped_probe_method._evomu_camera_guard = True
                return _wrapped_probe_method

            setattr(
                pt_cls,
                meth_name,
                _make_probe_id_guard(orig_meth_func, getattr(orig_meth_func, "__name__", meth_name)),
            )
            hooked = True

    if hooked:
        _HOOKED_CAMERA_GUARD_TARGETS.add("probeTracker")
    return hooked


def _install_live_client_sync_guards():
    """Install all live-client transition/sync guards (#1267, #1254, #1218, #1217, #1252, #1286, #1292, #1293, #1318, #1332, #1333, #1334, #1340, #1410)."""
    _install_base_space_camera_guard()
    _install_inventory_transition_guard()
    _install_fitting_active_ship_guard()
    _install_hangar_view_teardown_guard()
    _install_base_hangar_behaviour_guard()
    _install_modular_hangar_teardown_guard()
    _install_modular_capital_hangar_guard()
    _install_modular_hangar_camera_guard()
    _install_wreck_rotation_guard()
    _install_slots_container_sync_guard()
    _install_sensor_suite_gfx_guard()
    _install_skill_plan_panel_signal_guard()
    _install_probe_tracker_guard()


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
    _install_fsd_typelist_preload_hook(preload_typelist=False)
    _install_camera_dock_guard()
    _install_live_client_sync_guards()
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
        _install_fsd_typelist_preload_hook(preload_typelist=False)
        _install_camera_dock_guard()
        _install_live_client_sync_guards()
        for target_mod, target_key, install_fn in (
            ("eve.client.script.ui.camera.baseSpaceCamera", "baseSpaceCamera", _install_base_space_camera_guard),
            ("eve.client.script.environment.spaceObject.wreck", "wreck", _install_wreck_rotation_guard),
            ("eve.client.script.ui.skillPlan.skillPlanPanel", "skillPlanPanel", _install_skill_plan_panel_signal_guard),
            ("eve.client.script.environment.invCache", "invCacheContainer", _install_inventory_transition_guard),
            ("probescanning.probeTracker", "probeTracker", _install_probe_tracker_guard),
            ("shipfitting.fittingWarnings", "fittingWarnings", _install_fitting_active_ship_guard),
            ("eve.client.script.ui.shared.fittingScreen.warningIconCont", "warningIconCont", _install_fitting_active_ship_guard),
            ("eve.client.script.ui.shared.fittingScreen.statsPanel", "statsPanel", _install_fitting_active_ship_guard),
            ("eve.client.script.ui.shared.fittingScreen.fittingPanels.dronePanel", "dronePanel", _install_fitting_active_ship_guard),
        ):
            if fullname == target_mod and target_key not in _HOOKED_CAMERA_GUARD_TARGETS:
                uthread_mod = sys.modules.get("uthread")
                uthread_new = getattr(uthread_mod, "new", None) if uthread_mod is not None else None
                if callable(uthread_new):
                    try:
                        uthread_new(install_fn)
                    except Exception:
                        pass
        _maybe_schedule_driver_on_main_thread()
        return None


def install():
    global _META_HOOK_INSTALLED
    if _META_HOOK_INSTALLED:
        return
    _META_HOOK_INSTALLED = True
    sys.meta_path.insert(0, _EvomuMetaHook())
