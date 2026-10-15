"""
evomu_bridge.py — Client-side Python ServiceManager (`sm`) & `michelle` Transport Bridge.

Fixes & Hardening:
1. Full frame demultiplexer: Separates asynchronous `ServerEvent` notifications from
   `RpcResponse` frames and correlates pending RPCs strictly by `req_id`.
2. Enforces per-request timeouts (`DEFAULT_RPC_TIMEOUT = 10.0s`) and max frame size bounds.
3. Supports bound remote objects (`bound_id`) alongside standard service calls.
4. Hydrates `util.KeyVal` and `util.Rowset` in pure Python on the client.
"""

import collections
import inspect
import json
import socket
import struct
import threading

MAX_FRAME_SIZE = 4 * 1024 * 1024  # 4 MiB
DEFAULT_RPC_TIMEOUT = 10.0


class LocalKeyVal(dict):
    """Pure-Python drop-in replacement for EVE's `util.KeyVal`."""

    def __getattr__(self, key):
        try:
            return self[key]
        except KeyError:
            return None

    def __setattr__(self, key, value):
        self[key] = value


class LocalRowset(list):
    """Pure-Python drop-in replacement for EVE's `util.Rowset`."""

    def __init__(self, columns, rows):
        super(LocalRowset, self).__init__()
        self.header = columns
        for row_vals in rows:
            self.append(LocalKeyVal(zip(columns, row_vals)))


def to_pyrep_json(val):
    """Encodes a Python primitive/collection into the tagged `PyRep` JSON format expected by `eve-proto`."""
    if val is None:
        return {"$py": "None"}
    if isinstance(val, bool):
        return {"$py": "Bool", "val": val}
    if isinstance(val, int):
        return {"$py": "Integer", "val": val}
    if isinstance(val, float):
        return {"$py": "Real", "val": val}
    if isinstance(val, bytes):
        return {"$py": "Buffer", "val": list(val)}
    if isinstance(val, str):
        return {"$py": "String", "val": val}
    if isinstance(val, tuple):
        return {"$py": "Tuple", "val": [to_pyrep_json(x) for x in val]}
    if isinstance(val, list):
        return {"$py": "List", "val": [to_pyrep_json(x) for x in val]}
    if isinstance(val, dict):
        return {
            "$py": "Dict",
            "val": [[to_pyrep_json(k), to_pyrep_json(v)] for k, v in val.items()],
        }
    return {"$py": "String", "val": repr(val)}


def from_pyrep_json(node):
    """Hydrates a tagged `PyRep` JSON node from `EvoMu` into native client Python objects."""
    if not isinstance(node, dict) or "$py" not in node:
        return node
    tag = node["$py"]
    val = node.get("val")
    if tag == "None":
        return None
    if tag in ("Bool", "Integer", "Real", "String", "AsciiString"):
        return val
    if tag == "Buffer":
        return bytes(val)
    if tag == "Tuple":
        return tuple(from_pyrep_json(x) for x in val)
    if tag == "List":
        return [from_pyrep_json(x) for x in val]
    if tag == "Dict":
        return {from_pyrep_json(k): from_pyrep_json(v) for k, v in val}
    if tag == "KeyVal":
        return LocalKeyVal({k: from_pyrep_json(v) for k, v in val.items()})
    if tag == "Rowset":
        cols = val["columns"]
        rows = [[from_pyrep_json(cell) for cell in row] for row in val["rows"]]
        return LocalRowset(cols, rows)
    return val


class _PendingCall(object):
    def __init__(self):
        self.event = threading.Event()
        self.response = None
        self.error = None


class EvoMuBridgeConnection(object):
    def __init__(self, host="127.0.0.1", port=26001, event_dispatcher=None):
        self.host = host
        self.port = port
        self.sock = None
        self._req_id = 0
        self._write_lock = threading.Lock()
        self._pending_lock = threading.Lock()
        self._pending = {}  # req_id -> _PendingCall
        self.event_queue = collections.deque()
        self._event_dispatcher = event_dispatcher
        self._reader_thread = None
        self._closed = False

    def connect(self):
        self.sock = socket.create_connection((self.host, self.port), timeout=DEFAULT_RPC_TIMEOUT)
        self.sock.settimeout(None)
        self._closed = False
        self._reader_thread = threading.Thread(target=self._reader_loop, daemon=True)
        self._reader_thread.start()

    def close(self):
        self._closed = True
        if self.sock is not None:
            try:
                self.sock.close()
            except Exception:
                pass
            self.sock = None

    def process_incoming_frame(self, envelope):
        """Demultiplexes an incoming `BridgeEnvelope` dict into either `ServerEvent` or `RpcResponse`."""
        kind = envelope.get("kind")
        if kind == "ServerEvent":
            event_name = envelope.get("event")
            args = [from_pyrep_json(a) for a in envelope.get("args", [])]
            self.event_queue.append(( envelope.get("tick", 0), event_name, args))
            if self._event_dispatcher is not None:
                self._event_dispatcher(event_name, *args)
            return

        if kind == "RpcResponse":
            req_id = envelope.get("req_id")
            with self._pending_lock:
                slot = self._pending.pop(req_id, None)
            if slot is not None:
                slot.response = envelope.get("result", {})
                slot.event.set()

    def _reader_loop(self):
        try:
            while not self._closed and self.sock is not None:
                raw_len = self._recv_exact(4)
                resp_len = struct.unpack("<I", raw_len)[0]
                if resp_len == 0 or resp_len > MAX_FRAME_SIZE:
                    raise ValueError("Invalid bridge frame length: %d" % resp_len)
                resp_bytes = self._recv_exact(resp_len)
                envelope = json.loads(resp_bytes.decode("utf-8"))
                self.process_incoming_frame(envelope)
        except Exception as exc:
            self._fail_all_pending(exc)

    def _fail_all_pending(self, exc):
        with self._pending_lock:
            for slot in self._pending.values():
                slot.error = exc
                slot.event.set()
            self._pending.clear()

    def call_remote(
        self,
        service_name,
        method_name,
        args,
        kwargs,
        bound_id=None,
        timeout=DEFAULT_RPC_TIMEOUT,
    ):
        with self._write_lock:
            if self.sock is None:
                self.connect()
            self._req_id += 1
            req_id = self._req_id
            slot = _PendingCall()
            with self._pending_lock:
                self._pending[req_id] = slot

            stack = inspect.stack()
            caller = None
            if len(stack) > 2:
                frame_info = stack[2]
                caller = "%s:%d" % (frame_info[1], frame_info[2])

            envelope = {
                "kind": "RpcRequest",
                "req_id": req_id,
                "service": service_name,
                "bound_id": bound_id,
                "method": method_name,
                "args": [to_pyrep_json(a) for a in args],
                "kwargs": {k: to_pyrep_json(v) for k, v in kwargs.items()},
                "caller_trace": caller,
            }
            payload = json.dumps(envelope).encode("utf-8")
            if len(payload) > MAX_FRAME_SIZE:
                raise ValueError("Outbound RPC frame exceeds MAX_FRAME_SIZE")
            self.sock.sendall(struct.pack("<I", len(payload)) + payload)

        if not slot.event.wait(timeout=timeout):
            with self._pending_lock:
                self._pending.pop(req_id, None)
            raise TimeoutError(
                "Timed out waiting for RPC %s.%s (req_id=%d)"
                % (service_name, method_name, req_id)
            )

        if slot.error is not None:
            raise RuntimeError("Bridge connection failed: %s" % slot.error)

        result = slot.response or {}
        if "Ok" in result:
            return from_pyrep_json(result["Ok"])
        err = result.get("Err") or {}
        raise RuntimeError(
            "EvoMu RPC Error [%s]: %s" % (err.get("code"), err.get("message"))
        )

    def _recv_exact(self, n):
        buf = b""
        while len(buf) < n:
            chunk = self.sock.recv(n - len(buf))
            if not chunk:
                raise EOFError("Connection closed by EvoMu server")
            buf += chunk
        return buf


class RemoteServiceShim(object):
    def __init__(self, bridge, service_name, bound_id=None):
        self._bridge = bridge
        self._service_name = service_name
        self._bound_id = bound_id

    def bind(self, object_id):
        res = self._bridge.call_remote(
            self._service_name, "MachoBindObject", (object_id,), {}
        )
        bound_id = res[0] if isinstance(res, tuple) else res
        return RemoteServiceShim(self._bridge, self._service_name, bound_id=bound_id)

    def __getattr__(self, method_name):
        def _rpc_call(*args, **kwargs):
            return self._bridge.call_remote(
                self._service_name,
                method_name,
                args,
                kwargs,
                bound_id=self._bound_id,
            )

        return _rpc_call


def install_service_manager_hook(sm, host="127.0.0.1", port=26001):
    """Hooks EVE's global `sm.RemoteSvc` and `sm.ScatterEvent` to route through `EvoMu`."""
    scatter = getattr(sm, "ScatterEvent", None)
    bridge = EvoMuBridgeConnection(host, port, event_dispatcher=scatter)
    sm.RemoteSvc = lambda service_name: RemoteServiceShim(bridge, service_name)
    return bridge
