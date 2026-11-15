"""Lightweight harness trace schema, VirtualClock model, and golden extractor."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1


class VirtualClock:
    """Deterministic virtual simulation clock with TiDi scaling.

    Tracks `sim_ms`, `tidi_factor`, and discrete destiny ticks without floating-point
    drift across repeated small advances (`<= 100 ms` steps).
    """

    def __init__(
        self,
        *,
        initial_tick: int = 0,
        tick_ms: int = 1000,
        tidi_factor: float = 1.0,
        sim_ms: int = 0,
    ) -> None:
        if tick_ms <= 0:
            raise ValueError(f"tick_ms must be > 0, got {tick_ms}")
        if tidi_factor <= 0.0:
            raise ValueError(f"tidi_factor must be > 0, got {tidi_factor}")
        self.initial_tick = int(initial_tick)
        self.tick_ms = int(tick_ms)
        self.tidi_factor = float(tidi_factor)
        self.sim_ms = int(sim_ms)
        self._scaled_elapsed_ms: float = 0.0

    @property
    def current_tick(self) -> int:
        return self.initial_tick + int(self._scaled_elapsed_ms // self.tick_ms)

    def set_tidi(self, tidi_factor: float) -> None:
        if tidi_factor <= 0.0:
            raise ValueError(f"tidi_factor must be > 0, got {tidi_factor}")
        self.tidi_factor = float(tidi_factor)

    def advance(self, delta_ms: int) -> int:
        if delta_ms < 0:
            raise ValueError(f"virtual clock cannot move backwards (delta_ms={delta_ms})")
        self.sim_ms += int(delta_ms)
        # Round to nearest micro-ms scaled unit to keep 0.1/0.25/0.5 TiDi exact over many steps.
        self._scaled_elapsed_ms = round(self._scaled_elapsed_ms + delta_ms * self.tidi_factor, 6)
        return self.current_tick

    def to_metadata(self) -> dict[str, Any]:
        return {
            "initial_tick": self.initial_tick,
            "tick_ms": self.tick_ms,
            "tidi_factor": self.tidi_factor,
            "sim_ms": self.sim_ms,
        }


class TraceRecorder:
    """Builds a JSON-serializable trace of inbound RPCs, clock steps, and outbound frames."""

    def __init__(
        self,
        scenario: str,
        *,
        clock: VirtualClock | None = None,
        entity_aliases: dict[str, str] | None = None,
        sessions: dict[str, dict[str, Any]] | None = None,
    ) -> None:
        self.scenario = scenario
        self.clock = clock if clock is not None else VirtualClock()
        self.entity_aliases: dict[str, str] = dict(entity_aliases or {})
        self.sessions: dict[str, dict[str, Any]] = dict(sessions or {})
        self.events: list[dict[str, Any]] = []
        self._seq = 0

    def _next_seq(self) -> int:
        self._seq += 1
        return self._seq

    def record_rpc_in(
        self,
        session: str,
        service: str,
        method: str,
        *,
        bound_id: int | None = None,
        args: Any = None,
        sim_ms: int | None = None,
    ) -> dict[str, Any]:
        ev: dict[str, Any] = {
            "seq": self._next_seq(),
            "kind": "rpc_in",
            "session": session,
            "service": service,
            "method": method,
            "sim_ms": self.clock.sim_ms if sim_ms is None else int(sim_ms),
        }
        if bound_id is not None:
            ev["bound_id"] = int(bound_id)
        if args is not None:
            ev["args"] = args
        self.events.append(ev)
        return ev

    def record_clock_advance(self, delta_ms: int, *, tidi_factor: float | None = None) -> dict[str, Any]:
        if tidi_factor is not None:
            self.clock.set_tidi(tidi_factor)
        tick = self.clock.advance(delta_ms)
        ev = {
            "seq": self._next_seq(),
            "kind": "clock_advance",
            "delta_ms": int(delta_ms),
            "tidi_factor": self.clock.tidi_factor,
            "sim_ms": self.clock.sim_ms,
            "tick": tick,
        }
        self.events.append(ev)
        return ev

    def record_destiny_update(
        self,
        session: str,
        *,
        epoch: int,
        stamp: int,
        actions: list[dict[str, Any]],
        has_set_state: bool = False,
        wait_for_bubble: bool = False,
        sim_ms: int | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        ev: dict[str, Any] = {
            "seq": self._next_seq(),
            "kind": "out_frame",
            "frame_type": "DoDestinyUpdate",
            "session": session,
            "epoch": int(epoch),
            "stamp": int(stamp),
            "has_set_state": bool(has_set_state),
            "wait_for_bubble": bool(wait_for_bubble),
            "sim_ms": self.clock.sim_ms if sim_ms is None else int(sim_ms),
            "actions": actions,
        }
        if metadata:
            ev["metadata"] = metadata
        self.events.append(ev)
        return ev

    def record_out_frame(
        self,
        session: str,
        frame_type: str,
        *,
        sim_ms: int | None = None,
        **fields: Any,
    ) -> dict[str, Any]:
        ev: dict[str, Any] = {
            "seq": self._next_seq(),
            "kind": "out_frame",
            "frame_type": frame_type,
            "session": session,
            "sim_ms": self.clock.sim_ms if sim_ms is None else int(sim_ms),
            **fields,
        }
        self.events.append(ev)
        return ev

    def to_dict(self) -> dict[str, Any]:
        data: dict[str, Any] = {
            "schema_version": SCHEMA_VERSION,
            "scenario": self.scenario,
            "virtual_clock": {
                "initial_tick": self.clock.initial_tick,
                "tick_ms": self.clock.tick_ms,
                "tidi_factor": self.clock.tidi_factor,
            },
            "events": list(self.events),
        }
        if self.sessions:
            data["sessions"] = self.sessions
        if self.entity_aliases:
            data["entity_aliases"] = self.entity_aliases
        return data

    def save(self, path: str | Path) -> Path:
        return save_trace(self.to_dict(), path)


def validate_trace_schema(trace: dict[str, Any]) -> None:
    if not isinstance(trace, dict):
        raise ValueError("trace root must be a JSON object")
    version = trace.get("schema_version")
    if version != SCHEMA_VERSION:
        raise ValueError(f"unsupported schema_version={version!r}, expected {SCHEMA_VERSION}")
    scenario = trace.get("scenario")
    if not isinstance(scenario, str) or not scenario:
        raise ValueError("trace 'scenario' must be a non-empty string")
    events = trace.get("events")
    if not isinstance(events, list):
        raise ValueError("trace 'events' must be a list")
    for idx, ev in enumerate(events):
        if not isinstance(ev, dict):
            raise ValueError(f"events[{idx}] must be an object")
        if "kind" not in ev:
            raise ValueError(f"events[{idx}] missing required field 'kind'")


def load_trace(path: str | Path) -> dict[str, Any]:
    p = Path(path)
    data = json.loads(p.read_text(encoding="utf-8"))
    validate_trace_schema(data)
    return data


def save_trace(trace: dict[str, Any], path: str | Path) -> Path:
    validate_trace_schema(trace)
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(trace, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return p


def _normalize_entity(entity: Any, aliases: dict[str, str]) -> str | int:
    key = str(entity)
    if key in aliases:
        return aliases[key]
    return entity


def _resolve_base_stamp(trace: dict[str, Any], explicit_base: int | None) -> int:
    if explicit_base is not None:
        return int(explicit_base)
    if "base_stamp" in trace and trace["base_stamp"] is not None:
        return int(trace["base_stamp"])
    stamps: list[int] = []
    for ev in trace.get("events", []):
        if ev.get("kind") == "out_frame" and ev.get("frame_type") == "DoDestinyUpdate":
            if "stamp" in ev and ev["stamp"] is not None:
                stamps.append(int(ev["stamp"]))
            for act in ev.get("actions", []):
                if isinstance(act, dict) and act.get("stamp") is not None:
                    stamps.append(int(act["stamp"]))
    if stamps:
        return min(stamps)
    vc = trace.get("virtual_clock") or {}
    return int(vc.get("initial_tick", 0))


def extract_golden(
    trace: dict[str, Any],
    *,
    base_stamp: int | None = None,
    normalize_entities: bool = True,
) -> dict[str, Any]:
    """Replay a trace under VirtualClock and emit golden action/relative-stamp JSON."""
    validate_trace_schema(trace)
    vc_cfg = trace.get("virtual_clock") or {}
    clock = VirtualClock(
        initial_tick=int(vc_cfg.get("initial_tick", 0)),
        tick_ms=int(vc_cfg.get("tick_ms", 1000)),
        tidi_factor=float(vc_cfg.get("tidi_factor", 1.0)),
    )
    aliases = {str(k): str(v) for k, v in (trace.get("entity_aliases") or {}).items()} if normalize_entities else {}
    resolved_base = _resolve_base_stamp(trace, base_stamp)

    sessions: dict[str, list[dict[str, Any]]] = {}
    for ev in trace["events"]:
        kind = ev["kind"]
        if kind == "clock_advance":
            delta_ms = int(ev.get("delta_ms", 0))
            if "tidi_factor" in ev and ev["tidi_factor"] is not None:
                clock.set_tidi(float(ev["tidi_factor"]))
            clock.advance(delta_ms)
            continue

        if kind == "out_frame" and ev.get("frame_type") == "DoDestinyUpdate":
            sess = str(ev.get("session", "default"))
            frame_stamp = ev.get("stamp")
            bucket = sessions.setdefault(sess, [])
            for act in ev.get("actions", []):
                if not isinstance(act, dict):
                    raise ValueError(f"DoDestinyUpdate action entry must be an object, got {act!r}")
                act_name = str(act["action"])
                raw_stamp = act.get("stamp", frame_stamp)
                if raw_stamp is None:
                    raise ValueError(f"DoDestinyUpdate action {act_name!r} missing stamp")
                rel_stamp = int(raw_stamp) - resolved_base
                entities = [_normalize_entity(e, aliases) for e in act.get("entities", [])]
                bucket.append(
                    {
                        "rel_stamp": rel_stamp,
                        "action": act_name,
                        "entities": entities,
                    }
                )

    return {
        "schema_version": SCHEMA_VERSION,
        "scenario": trace["scenario"],
        "sessions": dict(sorted(sessions.items())),
    }
