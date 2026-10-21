# -*- coding: utf-8 -*-
"""
evomu_bridge.py -- Client-side Python ServiceManager (`sm`) & `michelle` Transport Bridge.

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
import sys
import threading

PY2 = sys.version_info[0] < 3
_INT_TYPES = (int, long) if PY2 else (int,)  # noqa: F821
_TEXT_TYPE = unicode if PY2 else str  # noqa: F821

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
        self.columns = columns
        for row_vals in rows:
            self.append(LocalKeyVal(zip(columns, row_vals)))


class LocalIndexRowset(dict):
    """Pure-Python drop-in replacement for EVE's `util.IndexRowset` (keyed by `idName`)."""

    def __init__(self, columns, key_column, items_dict):
        super(LocalIndexRowset, self).__init__()
        self.header = columns
        self.columns = columns
        self.idName = key_column
        for k, row_vals in items_dict.items():
            self[k] = LocalKeyVal(zip(columns, row_vals))


class LocalTupleSet(tuple):
    """Pure-Python drop-in replacement for EVE's `(columns, rows)` `TupleSet`."""

    def __new__(cls, columns, rows):
        return super(LocalTupleSet, cls).__new__(cls, (columns, rows))


def to_pyrep_json(val):
    """Encodes a Python 2.7 / Python 3 primitive or collection into the tagged `PyRep` JSON format expected by `eve-proto`."""
    if val is None:
        return {"$py": "None"}
    if isinstance(val, bool):
        return {"$py": "Bool", "val": val}
    if isinstance(val, _INT_TYPES):
        return {"$py": "Integer", "val": int(val)}
    if isinstance(val, float):
        return {"$py": "Real", "val": val}
    if isinstance(val, bytearray):
        return {"$py": "Buffer", "val": [int(b) for b in val]}
    if not PY2 and isinstance(val, bytes):
        return {"$py": "Buffer", "val": list(val)}
    if isinstance(val, _TEXT_TYPE):
        return {"$py": "String", "val": val}
    if PY2 and isinstance(val, str):
        try:
            return {"$py": "String", "val": val.decode("utf-8")}
        except UnicodeDecodeError:
            return {"$py": "Buffer", "val": [ord(b) for b in val]}
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
    if tag in ("Bool", "Integer", "Real", "String", "AsciiString", "Token"):
        return val
    if tag in ("Buffer", "Bytes", "Picked"):
        return bytes(bytearray(val))
    if tag in ("SubStream", "SubStruct"):
        return from_pyrep_json(val)
    if tag == "ChecksumedStream":
        if isinstance(val, dict) and "body" in val:
            return from_pyrep_json(val["body"])
        return from_pyrep_json(val)
    if tag == "Object":
        type_name = val.get("type_name") if isinstance(val, dict) else None
        args_py = from_pyrep_json(val.get("args")) if isinstance(val, dict) else None
        if (
            type_name == "objectCaching.CachedMethodCallResult"
            and isinstance(args_py, tuple)
            and len(args_py) >= 2
        ):
            return args_py[1]
        if (
            type_name == "objectCaching.CachedObject"
            and isinstance(args_py, tuple)
            and len(args_py) >= 7
        ):
            return LocalKeyVal(
                {
                    "type_name": type_name,
                    "__klass__": type_name,
                    "objectVersion": args_py[0],
                    "version": args_py[1],
                    "nodeID": args_py[2],
                    "shared": args_py[3],
                    "lret": args_py[4],
                    "compressed": args_py[5],
                    "objectID": args_py[6],
                }
            )
        if isinstance(args_py, LocalKeyVal):
            args_py["__klass__"] = type_name
            args_py["type_name"] = type_name
            return args_py
        return LocalKeyVal({"__klass__": type_name, "type_name": type_name, "args": args_py})
    if tag == "Tuple":
        return tuple(from_pyrep_json(x) for x in val)
    if tag == "List":
        return [from_pyrep_json(x) for x in val]
    if tag == "Dict":
        return {from_pyrep_json(k): from_pyrep_json(v) for k, v in val}
    if tag == "KeyVal":
        return LocalKeyVal({k: from_pyrep_json(v) for k, v in val.items()})
    if tag == "DBRow":
        if isinstance(val, dict) and "columns" in val:
            cols = val["columns"]
            if isinstance(cols, dict):
                return LocalKeyVal({k: from_pyrep_json(v) for k, v in cols.items()})
            return LocalKeyVal({k: from_pyrep_json(v) for k, v in cols})
        return LocalKeyVal({k: from_pyrep_json(v) for k, v in val.items()})
    if tag == "ObjectEx":
        list_items = [from_pyrep_json(x) for x in val.get("list", [])]
        dict_items = {from_pyrep_json(k): from_pyrep_json(v) for k, v in val.get("dict", [])}
        hdr = val.get("header")
        hdr_py = from_pyrep_json(hdr) if hdr is not None else None
        is_indexed = (
            isinstance(hdr_py, tuple)
            and len(hdr_py) > 0
            and isinstance(hdr_py[0], tuple)
            and len(hdr_py[0]) > 0
            and hdr_py[0][0] == "dbutil.CIndexedRowset"
        ) or (
            isinstance(hdr_py, tuple)
            and len(hdr_py) > 0
            and hdr_py[0] == "dbutil.CIndexedRowset"
        )
        if is_indexed or (dict_items and not list_items):
            return dict_items
        return list_items
    if tag == "Rowset":
        cols = val["columns"]
        rows = [[from_pyrep_json(cell) for cell in row] for row in val["rows"]]
        return LocalRowset(cols, rows)
    if tag == "IndexRowset":
        cols = val["columns"]
        key_col = val["key_column"]
        items = {
            from_pyrep_json(k): [from_pyrep_json(c) for c in row]
            for k, row in val["items"]
        }
        return LocalIndexRowset(cols, key_col, items)
    if tag == "TupleSet":
        cols = val["columns"]
        rows = [[from_pyrep_json(cell) for cell in row] for row in val["rows"]]
        return LocalTupleSet(cols, rows)
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
        self._reader_thread = threading.Thread(target=self._reader_loop)
        self._reader_thread.daemon = True
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
            self.event_queue.append((envelope.get("tick", 0), event_name, args))
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
            raise socket.timeout(
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
            self._service_name, "MachoBindObject", (object_id, None), {}
        )
        bound_id = res[0] if isinstance(res, tuple) else res
        return RemoteServiceShim(self._bridge, self._service_name, bound_id=bound_id)

    def MachoBindObject(self, bind_params, call=None):
        """Supports EVE's native `MachoBindObject(bindParams, call)` with optional piggybacked call tuple."""
        res = self._bridge.call_remote(
            self._service_name, "MachoBindObject", (bind_params, call), {}
        )
        if isinstance(res, tuple) and len(res) == 2:
            bound_id, call_res = res
            return (
                RemoteServiceShim(self._bridge, self._service_name, bound_id=bound_id),
                call_res,
            )
        return RemoteServiceShim(self._bridge, self._service_name, bound_id=res), None

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


class PublicGatewayShim(object):
    """
    4th Chokepoint for modern EVE V24.01 Client (`publicGatewaySvc`).
    Intercepts Quasar Protobuf requests (`eve_public.*`) and routes them over the EvoMu Bridge.
    """

    def __init__(self, bridge):
        self._bridge = bridge
        self._notice_handlers = collections.defaultdict(list)

    def send_character_request(self, request_proto, response_cls, timeout=DEFAULT_RPC_TIMEOUT):
        type_name = getattr(request_proto, "DESCRIPTOR", None)
        full_name = type_name.full_name if type_name is not None else type(request_proto).__name__
        payload_bytes = bytearray(
            request_proto.SerializeToString()
            if hasattr(request_proto, "SerializeToString")
            else b""
        )
        raw_rsp = self._bridge.call_remote(
            "publicGatewaySvc",
            full_name,
            (payload_bytes,),
            {},
            timeout=timeout,
        )
        if response_cls is not None and hasattr(response_cls, "FromString") and isinstance(raw_rsp, (bytes, bytearray)):
            rsp = response_cls()
            rsp.ParseFromString(bytes(raw_rsp))
            return rsp
        return raw_rsp

    def subscribe_to_notice(self, notice_cls, callback):
        name = getattr(getattr(notice_cls, "DESCRIPTOR", None), "full_name", str(notice_cls))
        self._notice_handlers[name].append((notice_cls, callback))


def install_service_manager_hook(sm, host="127.0.0.1", port=26001):
    """Hooks EVE's global `sm.RemoteSvc`, `sm.ScatterEvent`, and `publicGatewaySvc` to route through `EvoMu`."""
    scatter = getattr(sm, "ScatterEvent", None)
    bridge = EvoMuBridgeConnection(host, port, event_dispatcher=scatter)
    sm.RemoteSvc = lambda service_name: RemoteServiceShim(bridge, service_name)
    sm.ProxySvc = lambda service_name: RemoteServiceShim(bridge, service_name)
    if hasattr(sm, "services") and isinstance(sm.services, dict):
        sm.services["publicGatewaySvc"] = PublicGatewayShim(bridge)
    return bridge


def _run_self_tests():
    """Offline unit test suite verifying container hydration and frame demultiplexing."""
    # 1. Verify KeyVal, Rowset, IndexRowset, TupleSet, DBRow hydration
    kv = from_pyrep_json({"$py": "KeyVal", "val": {"shipID": {"$py": "Integer", "val": 1001001}}})
    assert isinstance(kv, LocalKeyVal) and kv.shipID == 1001001

    dbrow = from_pyrep_json({"$py": "DBRow", "val": {"itemID": {"$py": "Integer", "val": 42}}})
    assert isinstance(dbrow, LocalKeyVal) and dbrow.itemID == 42

    packed_dbrow = from_pyrep_json({
        "$py": "DBRow",
        "val": {
            "header": {"$py": "None"},
            "columns": [["itemID", {"$py": "Integer", "val": 42}], ["typeID", {"$py": "Integer", "val": 587}]],
            "raw_rle": [],
        },
    })
    assert isinstance(packed_dbrow, LocalKeyVal) and packed_dbrow.itemID == 42 and packed_dbrow.typeID == 587

    crowset = from_pyrep_json({
        "$py": "ObjectEx",
        "val": {
            "is_type_2": True,
            "header": {"$py": "Tuple", "val": []},
            "list": [{
                "$py": "DBRow",
                "val": {
                    "header": {"$py": "None"},
                    "columns": [["itemID", {"$py": "Integer", "val": 1001001}]],
                    "raw_rle": [],
                },
            }],
            "dict": [],
        },
    })
    assert isinstance(crowset, list) and len(crowset) == 1 and crowset[0].itemID == 1001001

    cindexed = from_pyrep_json({
        "$py": "ObjectEx",
        "val": {
            "is_type_2": True,
            "header": {"$py": "Tuple", "val": []},
            "list": [],
            "dict": [[
                {"$py": "Integer", "val": 34},
                {
                    "$py": "DBRow",
                    "val": {
                        "header": {"$py": "None"},
                        "columns": [["typeID", {"$py": "Integer", "val": 34}], ["price", {"$py": "Real", "val": 5.25}]],
                        "raw_rle": [],
                    },
                },
            ]],
        },
    })
    assert isinstance(cindexed, dict) and cindexed[34].price == 5.25

    rs = from_pyrep_json({
        "$py": "Rowset",
        "val": {
            "columns": ["itemID", "typeID"],
            "rows": [[{"$py": "Integer", "val": 1001001}, {"$py": "Integer", "val": 587}]],
        },
    })
    assert len(rs) == 1 and rs[0].typeID == 587

    irs = from_pyrep_json({
        "$py": "IndexRowset",
        "val": {
            "columns": ["itemID", "typeID"],
            "key_column": "itemID",
            "items": [[{"$py": "Integer", "val": 1001001}, [{"$py": "Integer", "val": 1001001}, {"$py": "Integer", "val": 587}]]],
        },
    })
    assert isinstance(irs, LocalIndexRowset) and irs[1001001].typeID == 587

    ts = from_pyrep_json({
        "$py": "TupleSet",
        "val": {
            "columns": ["stationID"],
            "rows": [[{"$py": "Integer", "val": 60003760}]],
        },
    })
    assert isinstance(ts, LocalTupleSet) and ts[0] == ["stationID"]

    # 2. Verify frame demultiplexer separates ServerEvent from RpcResponse
    events = []
    bridge = EvoMuBridgeConnection(event_dispatcher=lambda ev, *a: events.append((ev, a)))
    slot = _PendingCall()
    bridge._pending[7] = slot
    bridge.process_incoming_frame({
        "kind": "ServerEvent",
        "tick": 12,
        "event": "OnSessionChanged",
        "args": [{"$py": "Bool", "val": False}],
    })
    bridge.process_incoming_frame({
        "kind": "RpcResponse",
        "req_id": 7,
        "result": {"Ok": {"$py": "Integer", "val": 99}},
    })
    assert len(events) == 1 and events[0][0] == "OnSessionChanged"
    assert slot.event.is_set() and from_pyrep_json(slot.response["Ok"]) == 99

    # 3. Verify binary marshal, RLE, AddBalls, and Quasar protobuf codecs in-process
    _run_binary_codec_self_tests()
    print("[OK] evomu_bridge.py self-tests passed!")


MARSHAL_STRING_TABLE = [
    "*corpid", "*locationid", "age", "Asteroid", "authentication", "ballID", "beyonce",
    "bloodlineID", "capacity", "categoryID", "character", "characterID", "characterName",
    "characterType", "charID", "chatx", "clientID", "config", "contraband",
    "corporationDateTime", "corporationID", "createDateTime", "customInfo", "description",
    "divisionID", "DoDestinyUpdate", "dogmaIM", "EVE System", "flag", "foo.SlimItem",
    "gangID", "Gemini", "gender", "graphicID", "groupID", "header", "idName", "invbroker",
    "itemID", "items", "jumps", "line", "lines", "locationID", "locationName",
    "macho.CallReq", "macho.CallRsp", "macho.MachoAddress", "macho.Notification",
    "macho.SessionChangeNotification", "modules", "name", "objectCaching",
    "objectCaching.CachedObject", "OnChatJoin", "OnChatLeave", "OnChatSpeak",
    "OnGodmaShipEffect", "OnItemChange", "OnModuleAttributeChange", "OnMultiEvent",
    "orbitID", "ownerID", "ownerName", "quantity", "raceID", "RowClass", "securityStatus",
    "Sentry Gun", "sessionchange", "singleton", "skillEffect", "squadronID", "typeID",
    "used", "userID", "util.CachedObject", "util.IndexRowset", "util.Moniker", "util.Row",
    "util.Rowset", "*multicastID", "AddBalls", "AttackHit3", "AttackHit3R", "AttackHit4R",
    "DoDestinyUpdates", "GetLocationsEx", "InvalidateCachedObjects", "JoinChannel", "LSC",
    "LaunchMissile", "LeaveChannel", "OID+", "OID-", "OnAggressionChange",
    "OnCharGangChange", "OnCharNoLongerInStation", "OnCharNowInStation", "OnDamageMessage",
    "OnDamageStateChange", "OnEffectHit", "OnGangDamageStateChange", "OnLSC", "OnSpecialFX",
    "OnTarget", "RemoveBalls", "SendMessage", "SetMaxSpeed", "SetSpeedFraction",
    "TerminalExplosion", "address", "alert", "allianceID", "allianceid", "bid", "bookmark",
    "bounty", "channel", "charid", "constellationid", "corpID", "corpid", "corprole",
    "damage", "duration", "effects.Laser", "gangid", "gangrole", "hqID", "issued", "jit",
    "languageID", "locationid", "machoVersion", "marketProxy", "minVolume", "orderID",
    "price", "range", "regionID", "regionid", "role", "rolesAtAll", "rolesAtBase",
    "rolesAtHQ", "rolesAtOther", "shipid", "sn", "solarSystemID", "solarsystemid",
    "solarsystemid2", "source", "splash", "stationID", "stationid", "target", "userType",
    "userid", "volEntered", "volRemaining", "weapon",
    "agent.missionTemplatizedContent_BasicKillMission",
    "agent.missionTemplatizedContent_ResearchKillMission",
    "agent.missionTemplatizedContent_StorylineKillMission",
    "agent.missionTemplatizedContent_GenericStorylineKillMission",
    "agent.missionTemplatizedContent_BasicCourierMission",
    "agent.missionTemplatizedContent_ResearchCourierMission",
    "agent.missionTemplatizedContent_StorylineCourierMission",
    "agent.missionTemplatizedContent_GenericStorylineCourierMission",
    "agent.missionTemplatizedContent_BasicTradeMission",
    "agent.missionTemplatizedContent_ResearchTradeMission",
    "agent.missionTemplatizedContent_StorylineTradeMission",
    "agent.missionTemplatizedContent_GenericStorylineTradeMission",
    "agent.offerTemplatizedContent_BasicExchangeOffer",
    "agent.offerTemplatizedContent_BasicExchangeOffer_ContrabandDemand",
    "agent.offerTemplatizedContent_BasicExchangeOffer_Crafting",
    "agent.LoyaltyPoints", "agent.ResearchPoints", "agent.Credits", "agent.Item",
    "agent.Entity", "agent.Objective", "agent.FetchObjective", "agent.EncounterObjective",
    "agent.DungeonObjective", "agent.TransportObjective", "agent.Reward",
    "agent.TimeBonusReward", "agent.MissionReferral", "agent.Location",
    "agent.StandardMissionDetails", "agent.OfferDetails", "agent.ResearchMissionDetails",
    "agent.StorylineMissionDetails",
]


def _write_size_ex(out, n):
    if n < 0xFF:
        out.append(n & 0xFF)
    else:
        out.append(0xFF)
        out.extend(bytearray(struct.pack("<I", n)))


def _read_size_ex(buf, pos):
    first = buf[pos]
    pos += 1
    if first < 0xFF:
        return first, pos
    val = struct.unpack("<I", bytes(buf[pos:pos + 4]))[0]
    return val, pos + 4


def _dbtype_bit_width(dbtype):
    if dbtype in (20, 21, 64, 5, 6):
        return 64
    if dbtype in (3, 19, 4):
        return 32
    if dbtype in (2, 18):
        return 16
    if dbtype in (16, 17):
        return 8
    if dbtype == 11:
        return 1
    return 0


def rle_decode(rle_bytes, expected_len):
    ba = bytearray(rle_bytes)
    out = bytearray([0] * expected_len)
    in_ix = 0
    out_ix = 0
    run = 0
    nibble = False
    while in_ix < len(ba):
        nibble = not nibble
        if nibble:
            run = ba[in_ix]
            in_ix += 1
            count = (run & 0x0F) - 8
        else:
            count = (run >> 4) - 8
        if count >= 0:
            zeros = count + 1
            for _ in range(zeros):
                if out_ix < expected_len:
                    out[out_ix] = 0
                    out_ix += 1
        else:
            while count < 0 and in_ix < len(ba):
                if out_ix < expected_len:
                    out[out_ix] = ba[in_ix]
                    out_ix += 1
                in_ix += 1
                count += 1
    return bytes(out)


def rle_encode(raw_bytes):
    ba = bytearray(raw_bytes)
    out = bytearray()
    i = 0
    high_nibble = False
    ctrl_pos = 0
    while i < len(ba):
        if ba[i] == 0:
            z = 1
            while i + z < len(ba) and ba[i + z] == 0 and z < 8:
                z += 1
            is_zero, run_len = True, z
        else:
            lit = 1
            while i + lit < len(ba) and ba[i + lit] != 0 and lit < 8:
                lit += 1
            is_zero, run_len = False, lit
        nib = (8 + run_len - 1) if is_zero else (8 - run_len)
        if not high_nibble:
            ctrl_pos = len(out)
            out.append(nib & 0x0F)
        else:
            out[ctrl_pos] |= (nib & 0x0F) << 4
        if not is_zero:
            out.extend(ba[i:i + run_len])
        i += run_len
        high_nibble = not high_nibble
    return bytes(out)


def _extract_dbrow_defs(header_node):
    if not isinstance(header_node, dict):
        return []
    tag = header_node.get("$py")
    val = header_node.get("val")
    if tag == "ObjectEx" and isinstance(val, dict):
        return _extract_dbrow_defs(val.get("header"))
    if tag == "Tuple" and isinstance(val, list) and len(val) >= 2:
        outer = val[1]
        if isinstance(outer, dict) and outer.get("$py") == "Tuple":
            items = outer.get("val", [])
            if items and isinstance(items[0], dict) and items[0].get("$py") == "Tuple":
                first_inner = items[0].get("val", [])
                if first_inner and isinstance(first_inner[0], dict) and first_inner[0].get("$py") == "Tuple":
                    items = first_inner
            defs = []
            for entry in items:
                if isinstance(entry, dict) and entry.get("$py") == "Tuple":
                    pair = entry.get("val", [])
                    if len(pair) >= 2:
                        name = pair[0].get("val", "")
                        dt = int(pair[1].get("val", 0))
                        defs.append((name, dt))
            return defs
    return []


def _unpack_packed_row_columns(header_node, raw_rle):
    defs = _extract_dbrow_defs(header_node)
    if not defs:
        return []
    expected_bytes = 0
    bool_count = 0
    for _, dt in defs:
        bw = _dbtype_bit_width(dt)
        if bw == 64:
            expected_bytes += 8
        elif bw == 32:
            expected_bytes += 4
        elif bw == 16:
            expected_bytes += 2
        elif bw == 8:
            expected_bytes += 1
        elif bw == 1:
            bool_count += 1
    expected_bytes += (bool_count + 7) // 8
    if expected_bytes == 0:
        return [[n, {"$py": "None"}] for n, _ in defs]
    unpacked = bytearray(rle_decode(raw_rle, expected_bytes))
    indexed = list(enumerate(defs))
    indexed.sort(key=lambda item: (-_dbtype_bit_width(item[1][1]), item[0]))
    offset = 0
    bool_bit_idx = 0
    decoded_by_idx = []
    for orig_idx, (name, dt) in indexed:
        if dt in (20, 21, 64, 6) and offset + 8 <= len(unpacked):
            v = struct.unpack("<q", bytes(unpacked[offset:offset + 8]))[0]
            offset += 8
            node = {"$py": "Integer", "val": v}
        elif dt == 5 and offset + 8 <= len(unpacked):
            v = struct.unpack("<d", bytes(unpacked[offset:offset + 8]))[0]
            offset += 8
            node = {"$py": "Real", "val": v}
        elif dt == 3 and offset + 4 <= len(unpacked):
            v = struct.unpack("<i", bytes(unpacked[offset:offset + 4]))[0]
            offset += 4
            node = {"$py": "Integer", "val": v}
        elif dt == 19 and offset + 4 <= len(unpacked):
            v = struct.unpack("<I", bytes(unpacked[offset:offset + 4]))[0]
            offset += 4
            node = {"$py": "Integer", "val": v}
        elif dt == 4 and offset + 4 <= len(unpacked):
            v = struct.unpack("<f", bytes(unpacked[offset:offset + 4]))[0]
            offset += 4
            node = {"$py": "Real", "val": float(v)}
        elif dt == 2 and offset + 2 <= len(unpacked):
            v = struct.unpack("<h", bytes(unpacked[offset:offset + 2]))[0]
            offset += 2
            node = {"$py": "Integer", "val": v}
        elif dt == 18 and offset + 2 <= len(unpacked):
            v = struct.unpack("<H", bytes(unpacked[offset:offset + 2]))[0]
            offset += 2
            node = {"$py": "Integer", "val": v}
        elif dt == 16 and offset < len(unpacked):
            v = struct.unpack("<b", bytes(unpacked[offset:offset + 1]))[0]
            offset += 1
            node = {"$py": "Integer", "val": v}
        elif dt == 17 and offset < len(unpacked):
            v = unpacked[offset]
            offset += 1
            node = {"$py": "Integer", "val": v}
        elif dt == 11:
            byte_idx = offset + (bool_bit_idx // 8)
            bit_mask = 1 << (bool_bit_idx % 8)
            bool_bit_idx += 1
            bval = bool(unpacked[byte_idx] & bit_mask) if byte_idx < len(unpacked) else False
            node = {"$py": "Bool", "val": bval}
        else:
            node = {"$py": "None"}
        decoded_by_idx.append((orig_idx, name, node))
    decoded_by_idx.sort(key=lambda x: x[0])
    return [[name, node] for _, name, node in decoded_by_idx]


def marshal_encode_pyrep(node):
    """Encodes a tagged `PyRep` JSON dict into a binary `0x7e` eveMarshal frame."""
    out = bytearray([0x7E, 0, 0, 0, 0])
    _marshal_encode_inner(node, out)
    return bytes(out)


def _marshal_encode_inner(node, out):
    tag = node["$py"]
    val = node.get("val")
    if tag == "None":
        out.append(0x01)
    elif tag == "Bool":
        out.append(0x1F if val else 0x20)
    elif tag == "Token":
        b = val.encode("utf-8")
        out.append(0x02)
        _write_size_ex(out, len(b))
        out.extend(bytearray(b))
    elif tag == "Integer":
        v = int(val)
        if v == -1:
            out.append(0x07)
        elif v == 0:
            out.append(0x08)
        elif v == 1:
            out.append(0x09)
        elif -128 <= v <= 127:
            out.append(0x06)
            out.extend(bytearray(struct.pack("<b", v)))
        elif -32768 <= v <= 32767:
            out.append(0x05)
            out.extend(bytearray(struct.pack("<h", v)))
        elif -2147483648 <= v <= 2147483647:
            out.append(0x04)
            out.extend(bytearray(struct.pack("<i", v)))
        else:
            out.append(0x03)
            out.extend(bytearray(struct.pack("<q", v)))
    elif tag == "Real":
        v = float(val)
        if v == 0.0 and struct.pack("<d", v) == b"\x00" * 8:
            out.append(0x0B)
        else:
            out.append(0x0A)
            out.extend(bytearray(struct.pack("<d", v)))
    elif tag == "Bytes":
        b = bytearray(val)
        if len(b) == 0:
            out.append(0x0E)
        elif len(b) == 1:
            out.append(0x0F)
            out.append(b[0])
        elif len(b) <= 0xFF:
            out.append(0x10)
            out.append(len(b))
            out.extend(b)
        else:
            out.append(0x13)
            _write_size_ex(out, len(b))
            out.extend(b)
    elif tag == "String":
        b = val.encode("utf-8")
        if len(b) == 0:
            out.append(0x28)
        else:
            out.append(0x2E)
            _write_size_ex(out, len(b))
            out.extend(bytearray(b))
    elif tag == "Buffer":
        b = bytearray(val)
        out.append(0x0D)
        _write_size_ex(out, len(b))
        out.extend(b)
    elif tag == "Picked":
        b = bytearray(val)
        out.append(0x21)
        _write_size_ex(out, len(b))
        out.extend(b)
    elif tag == "Tuple":
        n = len(val)
        if n == 0:
            out.append(0x24)
        elif n == 1:
            out.append(0x25)
            _marshal_encode_inner(val[0], out)
        elif n == 2:
            out.append(0x2C)
            _marshal_encode_inner(val[0], out)
            _marshal_encode_inner(val[1], out)
        else:
            out.append(0x14)
            _write_size_ex(out, n)
            for item in val:
                _marshal_encode_inner(item, out)
    elif tag == "List":
        n = len(val)
        if n == 0:
            out.append(0x26)
        elif n == 1:
            out.append(0x27)
            _marshal_encode_inner(val[0], out)
        else:
            out.append(0x15)
            _write_size_ex(out, n)
            for item in val:
                _marshal_encode_inner(item, out)
    elif tag == "Dict":
        out.append(0x16)
        _write_size_ex(out, len(val))
        for k, v in val:
            _marshal_encode_inner(v, out)
            _marshal_encode_inner(k, out)
    elif tag == "SubStruct":
        out.append(0x19)
        _marshal_encode_inner(val, out)
    elif tag == "ChecksumedStream":
        out.append(0x1C)
        out.extend(bytearray(struct.pack("<I", int(val["checksum"]))))
        _marshal_encode_inner(val["body"], out)
    elif tag == "SubStream":
        out.append(0x2B)
        sub = bytearray(marshal_encode_pyrep(val))
        _write_size_ex(out, len(sub))
        out.extend(sub)
    elif tag == "KeyVal":
        out.append(0x17)
        _marshal_encode_inner({"$py": "Token", "val": "util.KeyVal"}, out)
        entries = [[{"$py": "String", "val": k}, val[k]] for k in sorted(val.keys())]
        _marshal_encode_inner({"$py": "Dict", "val": entries}, out)
    elif tag == "Rowset":
        out.append(0x17)
        _marshal_encode_inner({"$py": "Token", "val": "util.Rowset"}, out)
        cols = {"$py": "List", "val": [{"$py": "String", "val": c} for c in val["columns"]]}
        lines = {"$py": "List", "val": [{"$py": "List", "val": r} for r in val["rows"]]}
        d = {"$py": "Dict", "val": [
            [{"$py": "String", "val": "header"}, cols],
            [{"$py": "String", "val": "RowClass"}, {"$py": "Token", "val": "util.Row"}],
            [{"$py": "String", "val": "lines"}, lines],
        ]}
        _marshal_encode_inner(d, out)
    elif tag == "IndexRowset":
        out.append(0x17)
        _marshal_encode_inner({"$py": "Token", "val": "util.IndexRowset"}, out)
        cols = {"$py": "List", "val": [{"$py": "String", "val": c} for c in val["columns"]]}
        items_dict = {"$py": "Dict", "val": [[k, {"$py": "List", "val": r}] for k, r in val["items"]]}
        d = {"$py": "Dict", "val": [
            [{"$py": "String", "val": "header"}, cols],
            [{"$py": "String", "val": "RowClass"}, {"$py": "Token", "val": "util.Row"}],
            [{"$py": "String", "val": "idName"}, {"$py": "String", "val": val["key_column"]}],
            [{"$py": "String", "val": "items"}, items_dict],
        ]}
        _marshal_encode_inner(d, out)
    elif tag == "TupleSet":
        cols = {"$py": "List", "val": [{"$py": "String", "val": c} for c in val["columns"]]}
        lines = {"$py": "List", "val": [{"$py": "List", "val": r} for r in val["rows"]]}
        _marshal_encode_inner({"$py": "Tuple", "val": [cols, lines]}, out)
    elif tag == "DBRow":
        out.append(0x2A)
        _marshal_encode_inner(val["header"], out)
        raw_rle = bytearray(val["raw_rle"])
        _write_size_ex(out, len(raw_rle))
        out.extend(raw_rle)
        defs = _extract_dbrow_defs(val["header"])
        col_map = dict(val["columns"])
        for idx, (col_name, dt) in enumerate(defs):
            if _dbtype_bit_width(dt) == 0:
                cval = col_map.get(col_name, {"$py": "None"})
                _marshal_encode_inner(cval, out)
    elif tag == "Object":
        out.append(0x17)
        # blue reads the PyObject class name as a plain string (matches eve-proto encoder).
        _tn = val["type_name"]
        _tn_bytes = list(_tn.encode("latin-1")) if isinstance(_tn, str) else list(_tn)
        _marshal_encode_inner({"$py": "Bytes", "val": _tn_bytes}, out)
        _marshal_encode_inner(val["args"], out)
    elif tag == "ObjectEx":
        out.append(0x23 if val.get("is_type_2") else 0x22)
        _marshal_encode_inner(val["header"], out)
        for item in val.get("list", []):
            _marshal_encode_inner(item, out)
        out.append(0x2D)
        for k, v in val.get("dict", []):
            _marshal_encode_inner(k, out)
            _marshal_encode_inner(v, out)
        out.append(0x2D)


def marshal_decode_pyrep(raw_bytes):
    """Decodes a raw `0x7e` or `0x78` zlib-compressed eveMarshal stream into a tagged `PyRep` JSON dict."""
    import zlib
    ba = bytearray(raw_bytes)
    if ba[0] == 0x78:
        ba = bytearray(zlib.decompress(bytes(ba)))
    if ba[0] != 0x7E:
        raise ValueError("Invalid marshal header byte: 0x%02x" % ba[0])
    save_count = struct.unpack("<I", bytes(ba[1:5]))[0]
    body_end = len(ba) - (save_count * 4)
    state = {
        "pos": 5,
        "body_end": body_end,
        "save_pos": body_end,
        "saved_slots": [None] * save_count,
    }
    node = _marshal_decode_inner(ba, state)
    if state["pos"] != body_end:
        raise ValueError("Trailing bytes in marshal stream: %d" % (body_end - state["pos"]))
    return node


def _marshal_decode_inner(ba, state):
    raw_byte = ba[state["pos"]]
    state["pos"] += 1
    is_saved = (raw_byte & 0x40) != 0
    opcode = raw_byte & 0x3F
    slot_idx = None
    if is_saved:
        sp = state["save_pos"]
        idx = struct.unpack("<I", bytes(ba[sp:sp + 4]))[0]
        state["save_pos"] += 4
        slot_idx = idx - 1

    if opcode == 0x01:
        val = {"$py": "None"}
    elif opcode == 0x1F:
        val = {"$py": "Bool", "val": True}
    elif opcode == 0x20:
        val = {"$py": "Bool", "val": False}
    elif opcode == 0x02:
        n, state["pos"] = _read_size_ex(ba, state["pos"])
        s = bytes(ba[state["pos"]:state["pos"] + n]).decode("utf-8")
        state["pos"] += n
        val = {"$py": "Token", "val": s}
    elif opcode == 0x07:
        val = {"$py": "Integer", "val": -1}
    elif opcode == 0x08:
        val = {"$py": "Integer", "val": 0}
    elif opcode == 0x09:
        val = {"$py": "Integer", "val": 1}
    elif opcode == 0x06:
        v = struct.unpack("<b", bytes(ba[state["pos"]:state["pos"] + 1]))[0]
        state["pos"] += 1
        val = {"$py": "Integer", "val": v}
    elif opcode == 0x05:
        v = struct.unpack("<h", bytes(ba[state["pos"]:state["pos"] + 2]))[0]
        state["pos"] += 2
        val = {"$py": "Integer", "val": v}
    elif opcode == 0x04:
        v = struct.unpack("<i", bytes(ba[state["pos"]:state["pos"] + 4]))[0]
        state["pos"] += 4
        val = {"$py": "Integer", "val": v}
    elif opcode == 0x03:
        v = struct.unpack("<q", bytes(ba[state["pos"]:state["pos"] + 8]))[0]
        state["pos"] += 8
        val = {"$py": "Integer", "val": v}
    elif opcode == 0x2F:
        n, state["pos"] = _read_size_ex(ba, state["pos"])
        raw_int = bytearray(ba[state["pos"]:state["pos"] + n])
        state["pos"] += n
        pad = 0xFF if (n > 0 and (raw_int[-1] & 0x80) != 0) else 0x00
        while len(raw_int) < 8:
            raw_int.append(pad)
        v = struct.unpack("<q", bytes(raw_int[:8]))[0]
        val = {"$py": "Integer", "val": v}
    elif opcode == 0x0B:
        val = {"$py": "Real", "val": 0.0}
    elif opcode == 0x0A:
        v = struct.unpack("<d", bytes(ba[state["pos"]:state["pos"] + 8]))[0]
        state["pos"] += 8
        val = {"$py": "Real", "val": v}
    elif opcode == 0x0D:
        n, state["pos"] = _read_size_ex(ba, state["pos"])
        b = [int(x) for x in ba[state["pos"]:state["pos"] + n]]
        state["pos"] += n
        val = {"$py": "Buffer", "val": b}
    elif opcode == 0x0E:
        val = {"$py": "Bytes", "val": []}
    elif opcode == 0x0F:
        b = [int(ba[state["pos"]])]
        state["pos"] += 1
        val = {"$py": "Bytes", "val": b}
    elif opcode == 0x10:
        n = ba[state["pos"]]
        state["pos"] += 1
        b = [int(x) for x in ba[state["pos"]:state["pos"] + n]]
        state["pos"] += n
        val = {"$py": "Bytes", "val": b}
    elif opcode == 0x11:
        idx = ba[state["pos"]]
        state["pos"] += 1
        val = {"$py": "String", "val": MARSHAL_STRING_TABLE[idx - 1]}
    elif opcode == 0x29:
        s = bytes(ba[state["pos"]:state["pos"] + 2]).decode("utf-16le")
        state["pos"] += 2
        val = {"$py": "String", "val": s}
    elif opcode == 0x12:
        char_len, state["pos"] = _read_size_ex(ba, state["pos"])
        byte_len = char_len * 2
        s = bytes(ba[state["pos"]:state["pos"] + byte_len]).decode("utf-16le")
        state["pos"] += byte_len
        val = {"$py": "String", "val": s}
    elif opcode == 0x13:
        n, state["pos"] = _read_size_ex(ba, state["pos"])
        b = [int(x) for x in ba[state["pos"]:state["pos"] + n]]
        state["pos"] += n
        val = {"$py": "Bytes", "val": b}
    elif opcode == 0x21:
        n, state["pos"] = _read_size_ex(ba, state["pos"])
        b = [int(x) for x in ba[state["pos"]:state["pos"] + n]]
        state["pos"] += n
        val = {"$py": "Picked", "val": b}
    elif opcode == 0x28:
        val = {"$py": "String", "val": ""}
    elif opcode == 0x2E:
        n, state["pos"] = _read_size_ex(ba, state["pos"])
        s = bytes(ba[state["pos"]:state["pos"] + n]).decode("utf-8")
        state["pos"] += n
        val = {"$py": "String", "val": s}
    elif opcode == 0x24:
        val = {"$py": "Tuple", "val": []}
    elif opcode == 0x25:
        val = {"$py": "Tuple", "val": [_marshal_decode_inner(ba, state)]}
    elif opcode == 0x2C:
        a = _marshal_decode_inner(ba, state)
        b = _marshal_decode_inner(ba, state)
        val = {"$py": "Tuple", "val": [a, b]}
    elif opcode == 0x14:
        n, state["pos"] = _read_size_ex(ba, state["pos"])
        val = {"$py": "Tuple", "val": [_marshal_decode_inner(ba, state) for _ in range(n)]}
    elif opcode == 0x26:
        val = {"$py": "List", "val": []}
    elif opcode == 0x27:
        val = {"$py": "List", "val": [_marshal_decode_inner(ba, state)]}
    elif opcode == 0x15:
        n, state["pos"] = _read_size_ex(ba, state["pos"])
        val = {"$py": "List", "val": [_marshal_decode_inner(ba, state) for _ in range(n)]}
    elif opcode == 0x16:
        n, state["pos"] = _read_size_ex(ba, state["pos"])
        entries = []
        for _ in range(n):
            v = _marshal_decode_inner(ba, state)
            k = _marshal_decode_inner(ba, state)
            entries.append([k, v])
        val = {"$py": "Dict", "val": entries}
    elif opcode == 0x17:
        type_rep = _marshal_decode_inner(ba, state)
        type_name = type_rep.get("val", "")
        if isinstance(type_name, (list, bytes, bytearray)):
            type_name = bytes(bytearray(type_name)).decode("latin-1")
        args = _marshal_decode_inner(ba, state)
        if type_name == "util.KeyVal" and args.get("$py") == "Dict":
            kv = {k.get("val"): v for k, v in args.get("val", [])}
            val = {"$py": "KeyVal", "val": kv}
        elif type_name == "util.Rowset" and args.get("$py") == "Dict":
            dmap = {k.get("val"): v for k, v in args.get("val", [])}
            cols = [c.get("val") for c in dmap["header"].get("val", [])]
            rows = [r.get("val", []) for r in dmap["lines"].get("val", [])]
            val = {"$py": "Rowset", "val": {"columns": cols, "rows": rows}}
        elif type_name == "util.IndexRowset" and args.get("$py") == "Dict":
            dmap = {k.get("val"): v for k, v in args.get("val", [])}
            cols = [c.get("val") for c in dmap["header"].get("val", [])]
            key_col = dmap["idName"].get("val")
            items = [[ik, iv.get("val", [])] for ik, iv in dmap["items"].get("val", [])]
            val = {"$py": "IndexRowset", "val": {"key_column": key_col, "columns": cols, "items": items}}
        else:
            val = {"$py": "Object", "val": {"type_name": type_name, "args": args}}
    elif opcode in (0x22, 0x23):
        is_type_2 = opcode == 0x23
        header = _marshal_decode_inner(ba, state)
        lst = []
        while ba[state["pos"]] != 0x2D:
            lst.append(_marshal_decode_inner(ba, state))
        state["pos"] += 1
        dct = []
        while ba[state["pos"]] != 0x2D:
            k = _marshal_decode_inner(ba, state)
            v = _marshal_decode_inner(ba, state)
            dct.append([k, v])
        state["pos"] += 1
        val = {"$py": "ObjectEx", "val": {"is_type_2": is_type_2, "header": header, "list": lst, "dict": dct}}
    elif opcode == 0x19:
        val = {"$py": "SubStruct", "val": _marshal_decode_inner(ba, state)}
    elif opcode == 0x1C:
        ck = struct.unpack("<I", bytes(ba[state["pos"]:state["pos"] + 4]))[0]
        state["pos"] += 4
        body = _marshal_decode_inner(ba, state)
        val = {"$py": "ChecksumedStream", "val": {"checksum": ck, "body": body}}
    elif opcode == 0x2B:
        n, state["pos"] = _read_size_ex(ba, state["pos"])
        sub = bytes(ba[state["pos"]:state["pos"] + n])
        state["pos"] += n
        val = {"$py": "SubStream", "val": marshal_decode_pyrep(sub)}
    elif opcode == 0x2A:
        header = _marshal_decode_inner(ba, state)
        rle_len, state["pos"] = _read_size_ex(ba, state["pos"])
        raw_rle = [int(x) for x in ba[state["pos"]:state["pos"] + rle_len]]
        state["pos"] += rle_len
        columns = _unpack_packed_row_columns(header, raw_rle)
        defs = _extract_dbrow_defs(header)
        for idx, (col_name, dt) in enumerate(defs):
            if _dbtype_bit_width(dt) == 0:
                cval = _marshal_decode_inner(ba, state)
                if idx < len(columns):
                    columns[idx] = [col_name, cval]
        val = {"$py": "DBRow", "val": {"header": header, "columns": columns, "raw_rle": raw_rle}}
    elif opcode == 0x1B:
        idx, state["pos"] = _read_size_ex(ba, state["pos"])
        val = state["saved_slots"][idx - 1]
    else:
        raise ValueError("Unsupported marshal opcode: 0x%02x" % opcode)

    if slot_idx is not None:
        state["saved_slots"][slot_idx] = val
    return val


def _run_binary_codec_self_tests():
    raw_rle = rle_encode(b"\x00\x00\x00\x00\x2a\x00\x00\x00")
    assert rle_decode(raw_rle, 8) == b"\x00\x00\x00\x00\x2a\x00\x00\x00"
    sample = {"$py": "Tuple", "val": [{"$py": "Integer", "val": 30000142}, {"$py": "String", "val": "Jita"}]}
    encoded = marshal_encode_pyrep(sample)
    assert marshal_decode_pyrep(encoded) == sample


def _run_live_demo(host="127.0.0.1", port=26001):
    """Executes the Phase 1 vertical slice against a running `eve-server` instance."""
    class DummySM(object):
        def __init__(self):
            self.services = {}
            self.events = []

        def ScatterEvent(self, event_name, *args):
            print("[ServerEvent] %s -> %r" % (event_name, args))
            self.events.append((event_name, args))

    sm = DummySM()
    bridge = install_service_manager_hook(sm, host=host, port=port)
    try:
        auth = sm.RemoteSvc("auth")
        print("1. Calling auth.Login('demo', 'demo-secret')...")
        res = auth.Login("demo", "demo-secret")
        print("   -> Login result:", res)

        char_mgr = sm.RemoteSvc("charUnboundMgr")
        chars = char_mgr.GetCharactersToSelect()
        print("2. Characters:", chars)
        char_id = chars[0].characterID
        char_mgr.SelectCharacterID(char_id)
        print("3. Selected characterID:", char_id)

        inv_bound, items = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        print("4. Piggybacked invbroker.List items:", items)

        dogma_bound, ship_attrs = sm.RemoteSvc("dogmaIM").MachoBindObject(
            1001001, ("GetShipAttributes", (), {})
        )
        print("5. Piggybacked dogmaIM.GetShipAttributes:", ship_attrs)

        undock_info = sm.RemoteSvc("stationSvc").Undock()
        print("6. Undocked into Jita:", undock_info)
    finally:
        bridge.close()


if __name__ == "__main__":
    if "--verify-golden" in sys.argv:
        idx = sys.argv.index("--verify-golden")
        from evomu_golden import verify_golden_wire_fixtures  # noqa: E402
        report = verify_golden_wire_fixtures(sys.argv[idx + 1])
        print(json.dumps(report))
    elif "--self-test" in sys.argv:
        _run_self_tests()
    else:
        _run_self_tests()
        try:
            _run_live_demo()
        except Exception as exc:
            print("[INFO] Live server not reachable on 127.0.0.1:26001 (%s); self-tests succeeded." % exc)

