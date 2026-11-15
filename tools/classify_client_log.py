#!/usr/bin/env python3
"""Classify V24 EVE client destiny logs for stamp parity and history staleness.

Parses real V24 client log output from `eve/client/script/remote/michelle.pyj` and
`destiny/net/client/_ticker.pyj` / `_baseticker.pyj`, including:
  - `DoDestinyUpdate call for tick <N> containing <M> updates.  waitForBubble=<B>`
  - `------ History Dump <cur> -------` / `------ <Class> History Dump <cur> -------`
    blocks with `state waiting: yes|no` and `[ <stamp> ] <action> [(x <count>)]`
  - `Action: <action> <stamp> - current: <cur> <args>`
  - `Michelle discarded a state that was too old <entry> < <latest>`
  - `Failed to synchronize to <stamp>`

Computes `stale-at-action` (`stamp < current`) and `stale-at-history`
(`stamp < dump_current`) overall and per entity, and enforces `--expect parity|stale|any`.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
from dataclasses import dataclass, field
import json
from pathlib import Path
import re
import sys
from typing import Iterable, Sequence, TextIO

# `michelle.pyj::RealFlushState` and `_ticker.pyj::_real_flush_state` log
# `'Action: %12.12s' % funcName`, which truncates action names longer than 12 chars.
DESTINY_12_CHAR_ALIASES: dict[str, str] = {
    "GotoDirectio": "GotoDirection",
    "SetSpeedFrac": "SetSpeedFraction",
    "SetBallMassi": "SetBallMassive",
    "SetBallPosit": "SetBallPosition",
    "SetBallVeloc": "SetBallVelocity",
    "SetBallRadiu": "SetBallRadius",
    "SetBallInter": "SetBallInteractive",
    "SetBallHarmo": "SetBallHarmonic",
    "OnDamageStat": "OnDamageStateChange",
    "OnFleetDamag": "OnFleetDamageStateChange",
    "OnSlimItemCh": "OnSlimItemChange",
    "OnDroneState": "OnDroneStateChange",
    "OnActivating": "OnActivatingWarp",
    "OnDeactivati": "OnDeactivatingWarp",
    "OnClientCont": "OnClientControllerEvent",
    "OnDbuffUpdat": "OnDbuffUpdated",
    "OnDotVictimU": "OnDotVictimUpdated",
    "OnShipStateU": "OnShipStateUpdate",
    "OnDockingAcc": "OnDockingAccepted",
    "OnSovereignt": "OnSovereigntyChanged",
    "OnAudioActiv": "OnAudioActivated",
    "TerminalPlay": "TerminalPlayDestructionEffect",
    "PackagedActi": "PackagedAction",
}

RE_DO_DESTINY_UPDATE = re.compile(
    r"\bDoDestinyUpdate\s+call\s+for\s+tick\s+(\d+)\b"
    r"(?:.*?containing\s+(\d+)\s+updates?)?"
    r"(?:.*?waitForBubble\s*=\s*(\S+))?"
)
RE_DO_DESTINY_UPDATES_BATCH = re.compile(r"\bDoDestinyUpdates\s+call,\s+count=\s*(\d+)")
RE_HISTORY_DUMP_HDR = re.compile(
    r"(?:-{3,}\s+(?:[A-Za-z_][A-Za-z0-9_]*\s+)?History\s+Dump\s+(None|\d+)\s*-*"
    r"|(?:^|\s)History\s+Dump\s+(None|\d+)(?:\s+-+|\s*$))"
)
RE_STATE_WAITING = re.compile(r"\bstate\s+waiting:\s*(yes|no)\b", re.IGNORECASE)
RE_HISTORY_ENTRY = re.compile(
    r"\[\s*(\d+)\s*\]\s+([A-Za-z_][A-Za-z0-9_]*)(?:\s+(.*))?$"
)
RE_REPEAT_SUFFIX = re.compile(r"\(x\s*(\d+)\)\s*$")
RE_ACTION_LINE = re.compile(
    r"\bAction:\s*([A-Za-z_][A-Za-z0-9_]*)\s+(\d+)\s+-\s+current:\s*(None|\d+)(?:\s+(.*))?$"
)
RE_DISCARDED_OLD = re.compile(
    r"Michelle\s+discarded\s+a\s+state\s+that\s+was\s+too\s+old\s+(\d+)\s*<\s*(\d+)"
)
RE_FAILED_SYNC = re.compile(r"Failed\s+to\s+synchronize\s+to\s+(\d+)")

# Entity extraction patterns for Python 2/3 repr strings from EVE logs.
RE_ITEM_ID = re.compile(r"""['"]itemID['"]\s*:\s*(-?\d+)L?""")
RE_EGO_ID = re.compile(r"""['"]ego['"]\s*:\s*(-?\d+)L?""")
RE_NAMED_ENTITY = re.compile(r"\b(?:entity|entity_id|ballID|shipID|ego)\s*=\s*(-?\d+)L?")
RE_REMOVE_BALLS_LIST = re.compile(r"\[\s*(-?\d+L?(?:\s*,\s*-?\d+L?)*)\s*,?\s*\]")
RE_TUPLE_INTS = re.compile(r"^\s*\(\s*(.+?)\s*\)\s*$")
RE_INT_TOKEN = re.compile(r"^(-?\d+)L?$")


def normalize_action_name(raw_name: str) -> str:
    """Expand `%12.12s`-truncated michelle action names to canonical names."""
    return DESTINY_12_CHAR_ALIASES.get(raw_name, raw_name)


def _unique_preserve_order(items: Iterable[str]) -> list[str]:
    seen: set[str] = set()
    out: list[str] = []
    for item in items:
        if item not in seen:
            seen.add(item)
            out.append(item)
    return out


def extract_entities(action: str, args_str: str) -> list[str]:
    """Extract entity/ball IDs as strings from a logged action argument string."""
    if not args_str:
        return []

    named = RE_NAMED_ENTITY.findall(args_str)
    if named:
        return _unique_preserve_order(named)

    if action == "SetState":
        ego = RE_EGO_ID.findall(args_str)
        if ego:
            return _unique_preserve_order(ego)

    if action in ("AddBalls2", "AddBalls", "AddBall", "OnSlimItemChange", "SetState"):
        item_ids = RE_ITEM_ID.findall(args_str)
        if item_ids:
            return _unique_preserve_order(item_ids)

    if action == "RemoveBalls":
        m = RE_REMOVE_BALLS_LIST.search(args_str)
        if m:
            ids = [tok.strip().rstrip("L") for tok in m.group(1).split(",") if tok.strip()]
            if ids:
                return _unique_preserve_order(ids)

    # Parse leading tuple/list elements cleanly for single-ball and two-ball actions.
    inner = args_str.strip()
    m_tup = RE_TUPLE_INTS.match(inner)
    if m_tup:
        inner = m_tup.group(1)

    tokens = [tok.strip() for tok in inner.split(",")]
    if action == "OnTarget":
        # ('add', source_id, target_id) -> source_id (or first numeric token)
        for tok in tokens[1:3]:
            m_int = RE_INT_TOKEN.match(tok)
            if m_int:
                return [m_int.group(1)]
        return []

    if action == "Collision":
        found: list[str] = []
        for tok in tokens[:2]:
            m_int = RE_INT_TOKEN.match(tok)
            if m_int:
                found.append(m_int.group(1))
        return _unique_preserve_order(found)

    if action == "OnDockingAccepted":
        # ([sx, sy, sz], [ex, ey, ez], station_id)
        for tok in reversed(tokens):
            m_int = RE_INT_TOKEN.match(tok.rstrip(")]"))
            if m_int:
                return [m_int.group(1)]
        return []

    if tokens:
        first = tokens[0].lstrip("([")
        m_int = RE_INT_TOKEN.match(first)
        if m_int:
            return [m_int.group(1)]

    return []


@dataclass
class ActionRecord:
    file: str
    line: int
    action: str
    raw_action: str
    stamp: int
    current: int | None
    entities: list[str]

    @property
    def is_stale(self) -> bool:
        return self.current is not None and self.stamp < self.current


@dataclass
class HistoryEntryRecord:
    file: str
    line: int
    action: str
    raw_action: str
    stamp: int
    dump_current: int | None
    count: int
    entities: list[str]

    @property
    def is_stale(self) -> bool:
        return self.dump_current is not None and self.stamp < self.dump_current


@dataclass
class EntityStats:
    actions: int = 0
    stale_at_action: int = 0
    history_entries: int = 0
    stale_at_history: int = 0
    actions_by_name: dict[str, int] = field(default_factory=lambda: defaultdict(int))

    def to_dict(self) -> dict[str, object]:
        return {
            "actions": self.actions,
            "stale_at_action": self.stale_at_action,
            "history_entries": self.history_entries,
            "stale_at_history": self.stale_at_history,
            "actions_by_name": dict(sorted(self.actions_by_name.items())),
        }


@dataclass
class ClassificationReport:
    files: list[str]
    ticks_seen: int = 0
    first_tick: int | None = None
    last_tick: int | None = None
    destiny_update_batches: int = 0
    actions_total: int = 0
    history_dumps_seen: int = 0
    history_entries_total: int = 0
    stale_at_action: int = 0
    stale_at_history: int = 0
    discarded_old_states: int = 0
    desync_warnings: int = 0
    malformed_lines: int = 0
    malformed_details: list[dict[str, object]] = field(default_factory=list)
    per_entity: dict[str, EntityStats] = field(default_factory=dict)
    stale_action_events: list[dict[str, object]] = field(default_factory=list)
    stale_history_events: list[dict[str, object]] = field(default_factory=list)

    def evaluate(self, expect: str, *, allow_empty: bool = False) -> tuple[bool, str]:
        has_data = (self.actions_total > 0) or (self.ticks_seen > 0) or (self.history_entries_total > 0)
        if expect == "parity":
            if not has_data and not allow_empty:
                return False, "no destiny ticks, actions, or history entries parsed (pass --allow-empty to override)"
            if self.stale_at_action > 0:
                return False, f"stale-at-action={self.stale_at_action} > 0"
            return True, "parity ok (stale-at-action=0)"
        if expect == "stale":
            if self.stale_at_action > 0:
                return True, f"stale detected (stale-at-action={self.stale_at_action})"
            return False, "expected stale-at-action > 0, got 0"
        if expect == "any":
            return True, f"summary (stale-at-action={self.stale_at_action}, stale-at-history={self.stale_at_history})"
        raise ValueError(f"unsupported expect mode: {expect!r}")

    def to_dict(self, expect: str = "any", *, allow_empty: bool = False) -> dict[str, object]:
        passed, reason = self.evaluate(expect, allow_empty=allow_empty)
        return {
            "files": self.files,
            "ticks_seen": self.ticks_seen,
            "first_tick": self.first_tick,
            "last_tick": self.last_tick,
            "destiny_update_batches": self.destiny_update_batches,
            "actions_total": self.actions_total,
            "history_dumps_seen": self.history_dumps_seen,
            "history_entries_total": self.history_entries_total,
            "stale_at_action": self.stale_at_action,
            "stale_at_history": self.stale_at_history,
            "discarded_old_states": self.discarded_old_states,
            "desync_warnings": self.desync_warnings,
            "malformed_lines": self.malformed_lines,
            "malformed_details": self.malformed_details,
            "per_entity": {k: v.to_dict() for k, v in _sorted_entities(self.per_entity)},
            "stale_action_events": self.stale_action_events,
            "stale_history_events": self.stale_history_events,
            "expect": expect,
            "passed": passed,
            "reason": reason,
        }


def _sorted_entities(mapping: dict[str, EntityStats]) -> list[tuple[str, EntityStats]]:
    def sort_key(item: tuple[str, EntityStats]) -> tuple[int, int, str]:
        k = item[0]
        if k == "unknown":
            return (2, 0, k)
        try:
            return (0, int(k), k)
        except ValueError:
            return (1, 0, k)

    return sorted(mapping.items(), key=sort_key)


class ClientLogParser:
    """Stateful parser for V24 EVE client destiny logs."""

    def __init__(self) -> None:
        self.ticks: list[int] = []
        self.destiny_update_batches: int = 0
        self.history_dumps_seen: int = 0
        self.discarded_old_states: int = 0
        self.desync_warnings: int = 0
        self.actions: list[ActionRecord] = []
        self.history_entries: list[HistoryEntryRecord] = []
        self.malformed_details: list[dict[str, object]] = []

        self._in_history_dump: bool = False
        self._dump_current: int | None = None
        self._saw_bad_update_batch: bool = False

    def _record_malformed(self, file_label: str, line_no: int, line: str, reason: str) -> None:
        self.malformed_details.append(
            {
                "file": file_label,
                "line": line_no,
                "reason": reason,
                "text": line[:200],
            }
        )

    def feed_lines(self, lines: Iterable[str], file_label: str = "<stream>") -> None:
        for line_no, raw_line in enumerate(lines, start=1):
            line = raw_line.rstrip("\r\n")
            stripped = line.strip()

            if not stripped:
                self._in_history_dump = False
                self._dump_current = None
                self._saw_bad_update_batch = False
                continue

            if "Found update batch with" in line:
                self._saw_bad_update_batch = True
                self._in_history_dump = False
                continue

            # 1. History Dump header
            if "History Dump" in line:
                m_hdr = RE_HISTORY_DUMP_HDR.search(line)
                if m_hdr:
                    cur_tok = m_hdr.group(1) or m_hdr.group(2)
                    self._in_history_dump = True
                    self._dump_current = None if cur_tok == "None" else int(cur_tok)
                    self.history_dumps_seen += 1
                    self._saw_bad_update_batch = False
                    continue
                self._in_history_dump = False
                self._record_malformed(file_label, line_no, stripped, "malformed History Dump header")
                continue

            # 2. DoDestinyUpdate tick header
            if "DoDestinyUpdate call for tick" in line:
                self._in_history_dump = False
                self._saw_bad_update_batch = False
                m_tick = RE_DO_DESTINY_UPDATE.search(line)
                if m_tick:
                    self.ticks.append(int(m_tick.group(1)))
                else:
                    self._record_malformed(file_label, line_no, stripped, "malformed DoDestinyUpdate tick line")
                continue

            m_batch = RE_DO_DESTINY_UPDATES_BATCH.search(line)
            if m_batch:
                self._in_history_dump = False
                self._saw_bad_update_batch = False
                self.destiny_update_batches += 1
                continue

            # 3. Action: <name> <stamp> - current: <cur> [args]
            if "Action:" in line:
                self._in_history_dump = False
                m_act = RE_ACTION_LINE.search(line)
                if m_act:
                    self._saw_bad_update_batch = False
                    raw_action = m_act.group(1)
                    action = normalize_action_name(raw_action)
                    stamp = int(m_act.group(2))
                    cur_tok = m_act.group(3)
                    current = None if cur_tok == "None" else int(cur_tok)
                    args_str = (m_act.group(4) or "").strip()
                    entities = extract_entities(action, args_str) or ["unknown"]
                    self.actions.append(
                        ActionRecord(
                            file=file_label,
                            line=line_no,
                            action=action,
                            raw_action=raw_action,
                            stamp=stamp,
                            current=current,
                            entities=entities,
                        )
                    )
                    continue
                if self._saw_bad_update_batch:
                    # `michelle.pyj::DoDestinyUpdate` logs `Action: <raw_tuple>` after
                    # `Found update batch with N items and M timestamps` before `OnFatalDesync`.
                    self._saw_bad_update_batch = False
                    self.desync_warnings += 1
                    continue
                self._record_malformed(file_label, line_no, stripped, "malformed Action line")
                continue

            self._saw_bad_update_batch = False

            # 4. Inside History Dump block
            if self._in_history_dump:
                if RE_STATE_WAITING.search(line):
                    continue
                m_hist = RE_HISTORY_ENTRY.search(line)
                if m_hist:
                    stamp = int(m_hist.group(1))
                    raw_action = m_hist.group(2)
                    action = normalize_action_name(raw_action)
                    tail = (m_hist.group(3) or "").strip()
                    count = 1
                    m_rep = RE_REPEAT_SUFFIX.search(tail)
                    if m_rep:
                        count = int(m_rep.group(1))
                        tail = tail[: m_rep.start()].strip()
                    entities = extract_entities(action, tail)
                    self.history_entries.append(
                        HistoryEntryRecord(
                            file=file_label,
                            line=line_no,
                            action=action,
                            raw_action=raw_action,
                            stamp=stamp,
                            dump_current=self._dump_current,
                            count=count,
                            entities=entities,
                        )
                    )
                    continue
                if "[" in stripped and "]" in stripped:
                    self._record_malformed(file_label, line_no, stripped, "malformed History Dump entry")
                    continue
                # Any other non-matching line exits the History Dump block.
                self._in_history_dump = False

            # 5. Supplementary Michelle warnings
            if RE_DISCARDED_OLD.search(line):
                self.discarded_old_states += 1
                continue
            if RE_FAILED_SYNC.search(line):
                self.desync_warnings += 1
                continue

    def build_report(self, files: list[str]) -> ClassificationReport:
        report = ClassificationReport(
            files=files,
            ticks_seen=len(self.ticks),
            first_tick=self.ticks[0] if self.ticks else None,
            last_tick=self.ticks[-1] if self.ticks else None,
            destiny_update_batches=self.destiny_update_batches,
            actions_total=len(self.actions),
            history_dumps_seen=self.history_dumps_seen,
            discarded_old_states=self.discarded_old_states,
            desync_warnings=self.desync_warnings,
            malformed_lines=len(self.malformed_details),
            malformed_details=list(self.malformed_details),
        )

        # Build correlation index from `(stamp, canonical_action)` -> known entities
        # observed on `Action:` lines so `History Dump` entries (which omit args in V24)
        # can be attributed to entities when available.
        action_entities_by_key: dict[tuple[int, str], list[str]] = defaultdict(list)
        per_entity: dict[str, EntityStats] = defaultdict(EntityStats)

        for act in self.actions:
            is_stale = act.is_stale
            if is_stale:
                report.stale_at_action += 1
                report.stale_action_events.append(
                    {
                        "file": act.file,
                        "line": act.line,
                        "action": act.action,
                        "stamp": act.stamp,
                        "current": act.current,
                        "delta": act.stamp - act.current if act.current is not None else 0,
                        "entities": act.entities,
                    }
                )
            for ent in act.entities:
                if ent != "unknown":
                    action_entities_by_key[(act.stamp, act.action)].append(ent)
                stats = per_entity[ent]
                stats.actions += 1
                stats.actions_by_name[act.action] += 1
                if is_stale:
                    stats.stale_at_action += 1

        for hent in self.history_entries:
            report.history_entries_total += hent.count
            entities = hent.entities
            if not entities:
                matched = _unique_preserve_order(action_entities_by_key.get((hent.stamp, hent.action), []))
                entities = matched if matched else ["unknown"]

            is_stale = hent.is_stale
            if is_stale:
                report.stale_at_history += hent.count
                report.stale_history_events.append(
                    {
                        "file": hent.file,
                        "line": hent.line,
                        "action": hent.action,
                        "stamp": hent.stamp,
                        "current": hent.dump_current,
                        "delta": hent.stamp - hent.dump_current if hent.dump_current is not None else 0,
                        "count": hent.count,
                        "entities": entities,
                    }
                )
            for ent in entities:
                stats = per_entity[ent]
                stats.history_entries += hent.count
                if is_stale:
                    stats.stale_at_history += hent.count

        report.per_entity = dict(per_entity)
        return report


def classify_logs(paths: Sequence[str], *, stdin: TextIO | None = None) -> ClassificationReport:
    """Parse one or more client log paths (or '-' for stdin) and return a ClassificationReport."""
    parser = ClientLogParser()
    labels: list[str] = []
    for raw_path in paths:
        if raw_path == "-":
            labels.append("<stdin>")
            src = stdin if stdin is not None else sys.stdin
            parser.feed_lines(src, "<stdin>")
        else:
            p = Path(raw_path)
            labels.append(str(p))
            with p.open("r", encoding="utf-8", errors="replace") as fh:
                parser.feed_lines(fh, str(p))
    return parser.build_report(labels)


def format_text_report(
    report: ClassificationReport,
    *,
    expect: str,
    allow_empty: bool = False,
    verbose: bool = False,
) -> str:
    passed, reason = report.evaluate(expect, allow_empty=allow_empty)
    verdict = "PASS" if passed else "FAIL"
    lines = [
        f"[{verdict}] {reason}",
        (
            f"Summary: ticks={report.ticks_seen} actions={report.actions_total} "
            f"history_dumps={report.history_dumps_seen} history_entries={report.history_entries_total} "
            f"stale-at-action={report.stale_at_action} stale-at-history={report.stale_at_history} "
            f"discarded_old={report.discarded_old_states} desyncs={report.desync_warnings} "
            f"malformed={report.malformed_lines}"
        ),
    ]
    if report.per_entity:
        lines.append("Per entity:")
        for ent, stats in _sorted_entities(report.per_entity):
            lines.append(
                f"  entity={ent}: actions={stats.actions} stale-at-action={stats.stale_at_action} "
                f"history_entries={stats.history_entries} stale-at-history={stats.stale_at_history}"
            )
    if verbose and report.stale_action_events:
        lines.append("Stale action events:")
        for ev in report.stale_action_events:
            lines.append(
                f"  {ev['file']}:{ev['line']} {ev['action']} stamp={ev['stamp']} "
                f"current={ev['current']} delta={ev['delta']} entities={ev['entities']}"
            )
    if verbose and report.stale_history_events:
        lines.append("Stale history events:")
        for ev in report.stale_history_events:
            lines.append(
                f"  {ev['file']}:{ev['line']} {ev['action']} stamp={ev['stamp']} "
                f"current={ev['current']} count={ev['count']} entities={ev['entities']}"
            )
    return "\n".join(lines)


def build_arg_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description="Classify V24 EVE client destiny logs for stale-at-action and stale-at-history events."
    )
    p.add_argument(
        "logs",
        nargs="+",
        help="Path(s) to client log file(s), or '-' for stdin.",
    )
    p.add_argument(
        "--expect",
        choices=("parity", "stale", "any"),
        default="parity",
        help="Expectation mode controlling exit code (default: parity).",
    )
    p.add_argument(
        "--allow-empty",
        action="store_true",
        help="Allow empty logs with 0 parsed ticks/actions/history entries to pass --expect parity.",
    )
    p.add_argument(
        "--format",
        choices=("text", "json"),
        default="text",
        help="Output format (default: text).",
    )
    p.add_argument(
        "--json",
        action="store_true",
        help="Shorthand for --format json.",
    )
    p.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        help="Include individual stale event details in text output.",
    )
    return p


def main(argv: Sequence[str] | None = None) -> int:
    args = build_arg_parser().parse_args(argv)
    out_format = "json" if args.json else args.format
    try:
        report = classify_logs(args.logs)
    except OSError as exc:
        print(f"error reading log file: {exc}", file=sys.stderr)
        return 2

    passed, _ = report.evaluate(args.expect, allow_empty=args.allow_empty)
    if out_format == "json":
        print(json.dumps(report.to_dict(args.expect, allow_empty=args.allow_empty), indent=2))
    else:
        print(format_text_report(report, expect=args.expect, allow_empty=args.allow_empty, verbose=args.verbose))
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
