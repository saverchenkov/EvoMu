#!/usr/bin/env python3
"""Replay a captured harness trace under VirtualClock, verify destiny invariants, and diff against golden JSON."""

from __future__ import annotations

import argparse
from collections import defaultdict
import difflib
import json
import os
from pathlib import Path
import sys
from typing import Any, Sequence

THIS_DIR = Path(__file__).resolve().parent
REPO_ROOT = THIS_DIR.parents[1]
GOLDEN_DIR = REPO_ROOT / "crates" / "eve-server" / "tests" / "golden"

if str(THIS_DIR.parent) not in sys.path:
    sys.path.insert(0, str(THIS_DIR.parent))

from replay.trace import VirtualClock, extract_golden, load_trace, validate_trace_schema  # noqa: E402

SPAWN_ACTIONS = frozenset({"SetState", "AddBall", "AddBalls", "AddBalls2"})
REMOVE_ACTIONS = frozenset({"RemoveBall", "RemoveBalls"})
MOVEMENT_ACTIONS = frozenset(
    {
        "GotoDirection",
        "GotoPoint",
        "FollowBall",
        "Orbit",
        "WarpTo",
        "Stop",
        "SetSpeedFraction",
    }
)
MULTI_INSTANCE_ALLOWED_ACTIONS = frozenset({"OnSpecialFX"})


def _entity_key(entity: Any, aliases: dict[str, str]) -> str:
    raw = str(entity)
    return aliases.get(raw, raw)


def check_destiny_invariants(trace: dict[str, Any], *, max_observer_lead: int = 2) -> list[str]:
    """Validate virtual-clock monotonicity and V24 destiny/motion invariants on a trace."""
    validate_trace_schema(trace)
    violations: list[str] = []

    vc_cfg = trace.get("virtual_clock") or {}
    clock = VirtualClock(
        initial_tick=int(vc_cfg.get("initial_tick", 0)),
        tick_ms=int(vc_cfg.get("tick_ms", 1000)),
        tidi_factor=float(vc_cfg.get("tidi_factor", 1.0)),
    )
    aliases = {str(k): str(v) for k, v in (trace.get("entity_aliases") or {}).items()}
    prev_sim_ms: int | None = None

    # Per-(session, epoch) state
    last_frame_stamp: dict[tuple[str, int], int] = {}
    saw_set_state: dict[tuple[str, int], bool] = defaultdict(bool)
    known_entities: dict[tuple[str, int], set[str]] = defaultdict(set)
    set_state_tracks_entities: dict[tuple[str, int], bool] = defaultdict(bool)
    removed_entities: dict[tuple[str, int], set[str]] = defaultdict(set)
    first_update_seen: dict[tuple[str, int, str], tuple[int, str]] = {}

    # Cross-action causality tracking
    last_owner_move_stamp: dict[str, int] = {}
    destruction_order: dict[tuple[str, int, str], tuple[int, int]] = {}
    removal_order: dict[tuple[str, int, str], tuple[int, int]] = {}
    deactivating_warp_stamp: dict[tuple[str, int], int] = {}
    correlated_stamps: dict[str, dict[str, int]] = defaultdict(dict)

    action_seq = 0
    for idx, ev in enumerate(trace["events"]):
        kind = ev["kind"]
        sim_ms = ev.get("sim_ms")
        if sim_ms is not None:
            sim_ms_int = int(sim_ms)
            if prev_sim_ms is not None and sim_ms_int < prev_sim_ms:
                violations.append(
                    f"events[{idx}] virtual clock sim_ms regressed: {sim_ms_int} < {prev_sim_ms}"
                )
            prev_sim_ms = sim_ms_int

        if kind == "clock_advance":
            delta_ms = int(ev.get("delta_ms", 0))
            if delta_ms < 0:
                violations.append(f"events[{idx}] negative clock_advance delta_ms={delta_ms}")
                continue
            if ev.get("tidi_factor") is not None:
                tidi = float(ev["tidi_factor"])
                if tidi <= 0.0:
                    violations.append(f"events[{idx}] non-positive tidi_factor={tidi}")
                    continue
                clock.set_tidi(tidi)
            expected_tick = clock.advance(delta_ms)
            if ev.get("tick") is not None and int(ev["tick"]) != expected_tick:
                violations.append(
                    f"events[{idx}] virtual clock tick mismatch: trace={ev['tick']} != expected={expected_tick}"
                )
            continue

        if kind != "out_frame" or ev.get("frame_type") != "DoDestinyUpdate":
            continue

        session = str(ev.get("session", "default"))
        epoch = int(ev.get("epoch", 1))
        frame_stamp = int(ev["stamp"])
        stream_key = (session, epoch)

        prev_stamp = last_frame_stamp.get(stream_key)
        if prev_stamp is not None and frame_stamp < prev_stamp:
            violations.append(
                f"session={session} epoch={epoch}: non-monotonic DoDestinyUpdate stamp "
                f"{frame_stamp} < {prev_stamp}"
            )
        last_frame_stamp[stream_key] = frame_stamp

        frame_seen_keys: set[tuple[str, str, str]] = set()

        for act in ev.get("actions", []):
            action_seq += 1
            act_name = str(act["action"])
            act_stamp = int(act.get("stamp", frame_stamp))
            act_meta = act.get("metadata") or {}
            raw_entities = act.get("entities", [])
            entities = [_entity_key(e, aliases) for e in raw_entities]

            if prev_stamp is not None and act_stamp < prev_stamp:
                violations.append(
                    f"session={session} epoch={epoch}: action {act_name} stamp {act_stamp} "
                    f"behind prior frame stamp {prev_stamp}"
                )

            if act_meta.get("correlation_id") is not None:
                corr_id = str(act_meta["correlation_id"])
                correlated_stamps[corr_id][session] = act_stamp

            if act_name == "SetState":
                saw_set_state[stream_key] = True
                removed_entities[stream_key].clear()
                if entities:
                    set_state_tracks_entities[stream_key] = True
                    known_entities[stream_key].update(entities)
                continue

            # Check duplicate (action, entity) in one frame
            for ent in entities:
                if act_name in MULTI_INSTANCE_ALLOWED_ACTIONS and (
                    act_meta.get("module_id") is not None or act_meta.get("guid") is not None
                ):
                    dedup_key = (act_name, ent, f"{act_meta.get('module_id')}:{act_meta.get('guid')}")
                else:
                    dedup_key = (act_name, ent, "")
                if dedup_key in frame_seen_keys:
                    violations.append(
                        f"session={session} epoch={epoch} stamp={frame_stamp}: "
                        f"duplicate action {act_name} for entity={ent} in same frame"
                    )
                frame_seen_keys.add(dedup_key)

            if act_name in SPAWN_ACTIONS:
                for ent in entities:
                    removed_entities[stream_key].discard(ent)
                    known_entities[stream_key].add(ent)
                    prior = first_update_seen.get((session, epoch, ent))
                    if prior is not None and not act_meta.get("respawn"):
                        prior_stamp, prior_act = prior
                        violations.append(
                            f"session={session} epoch={epoch}: entity={ent} received {prior_act} "
                            f"at stamp={prior_stamp} before {act_name} at stamp={act_stamp}"
                        )

                # Missile AddBalls2 stamp relative to owner movement stamp
                missile_owner = act_meta.get("missile_owner")
                if missile_owner is not None:
                    owner_key = _entity_key(missile_owner, aliases)
                    owner_mv = last_owner_move_stamp.get(owner_key)
                    if owner_mv is not None:
                        if act_stamp < owner_mv or act_stamp > owner_mv + 2:
                            violations.append(
                                f"session={session} stamp={act_stamp}: missile {act_name} stamp "
                                f"outside [owner_move={owner_mv}, {owner_mv + 2}] for owner={owner_key}"
                            )

                # Destination AddBalls2 vs OnDeactivatingWarp stamp
                if act_meta.get("phase") == "warp_arrival" or act_meta.get("warp_arrival"):
                    deact = deactivating_warp_stamp.get(stream_key)
                    if deact is not None and act_stamp < deact:
                        violations.append(
                            f"session={session} epoch={epoch}: destination {act_name} stamp={act_stamp} "
                            f"is behind OnDeactivatingWarp stamp={deact}"
                        )

                # Wreck AddBalls2 vs victim RemoveBalls
                wreck_of = act_meta.get("wreck_of")
                if wreck_of is not None:
                    victim_key = _entity_key(wreck_of, aliases)
                    rem_pos = removal_order.get((session, epoch, victim_key))
                    if rem_pos is None:
                        violations.append(
                            f"session={session} epoch={epoch}: wreck {act_name} for victim={victim_key} "
                            f"emitted before victim RemoveBalls"
                        )
                    elif (act_stamp, action_seq) <= rem_pos:
                        violations.append(
                            f"session={session} epoch={epoch}: wreck {act_name} {(act_stamp, action_seq)} "
                            f"must follow victim RemoveBalls {rem_pos}"
                        )

            elif act_name in REMOVE_ACTIONS:
                for ent in entities:
                    term_pos = destruction_order.get((session, epoch, ent))
                    if term_pos is not None and (act_stamp, action_seq) <= term_pos:
                        violations.append(
                            f"session={session} epoch={epoch}: {act_name} for entity={ent} "
                            f"must follow TerminalPlayDestructionEffect"
                        )
                    removal_order[(session, epoch, ent)] = (act_stamp, action_seq)
                    known_entities[stream_key].discard(ent)
                    removed_entities[stream_key].add(ent)

            else:
                for ent in entities:
                    if ent in removed_entities[stream_key]:
                        violations.append(
                            f"session={session} epoch={epoch} stamp={act_stamp}: "
                            f"entity={ent} received {act_name} after RemoveBalls"
                        )
                    if (
                        saw_set_state[stream_key]
                        and set_state_tracks_entities[stream_key]
                        and ent not in known_entities[stream_key]
                    ):
                        violations.append(
                            f"session={session} epoch={epoch} stamp={act_stamp}: "
                            f"entity={ent} received {act_name} before AddBalls2"
                        )
                    first_update_seen.setdefault((session, epoch, ent), (act_stamp, act_name))

                    if act_name in MOVEMENT_ACTIONS:
                        last_owner_move_stamp[ent] = act_stamp
                    if act_name == "TerminalPlayDestructionEffect":
                        destruction_order[(session, epoch, ent)] = (act_stamp, action_seq)
                    if act_name == "OnDeactivatingWarp":
                        deactivating_warp_stamp[stream_key] = act_stamp

    for corr_id, sess_stamps in correlated_stamps.items():
        if "owner" in sess_stamps and "observer" in sess_stamps:
            owner_st = sess_stamps["owner"]
            obs_st = sess_stamps["observer"]
            if owner_st > obs_st + max_observer_lead:
                violations.append(
                    f"correlation_id={corr_id}: owner stamp {owner_st} > "
                    f"observer stamp {obs_st} + lead {max_observer_lead}"
                )

    return violations


def compare_golden(
    actual_golden: dict[str, Any],
    golden_path: str | Path,
    *,
    update_golden: bool = False,
) -> tuple[bool, str]:
    """Compare extracted golden JSON against committed golden_path (or update if UPDATE_GOLDEN=1)."""
    gpath = Path(golden_path)
    formatted_actual = json.dumps(actual_golden, indent=2, sort_keys=True) + "\n"

    if update_golden:
        gpath.parent.mkdir(parents=True, exist_ok=True)
        gpath.write_text(formatted_actual, encoding="utf-8")
        return True, f"updated golden file: {gpath}"

    if not gpath.is_file():
        return False, f"missing golden file: {gpath} (set UPDATE_GOLDEN=1 to generate)"

    expected_obj = json.loads(gpath.read_text(encoding="utf-8"))
    if expected_obj == actual_golden:
        return True, f"golden match: {gpath}"

    formatted_expected = json.dumps(expected_obj, indent=2, sort_keys=True) + "\n"
    diff = "".join(
        difflib.unified_diff(
            formatted_expected.splitlines(keepends=True),
            formatted_actual.splitlines(keepends=True),
            fromfile=f"expected:{gpath}",
            tofile="actual:replay",
        )
    )
    return False, f"golden mismatch for {gpath}:\n{diff}"


def verify_trace_data(
    trace: dict[str, Any],
    golden_path: str | Path,
    *,
    update_golden: bool = False,
    check_invariants: bool = True,
    base_stamp: int | None = None,
    max_observer_lead: int = 2,
) -> tuple[bool, dict[str, Any]]:
    """Run invariant checks + golden sequence verification on an in-memory trace dict."""
    violations: list[str] = []
    if check_invariants:
        violations = check_destiny_invariants(trace, max_observer_lead=max_observer_lead)

    actual_golden = extract_golden(trace, base_stamp=base_stamp)
    golden_ok, golden_msg = compare_golden(
        actual_golden, golden_path, update_golden=update_golden
    )
    passed = (not violations) and golden_ok
    result = {
        "scenario": trace.get("scenario"),
        "passed": passed,
        "invariants_checked": check_invariants,
        "invariant_violations": violations,
        "golden_ok": golden_ok,
        "golden_message": golden_msg,
        "golden_path": str(golden_path),
        "actual_golden": actual_golden,
    }
    return passed, result


def verify_trace_file(
    trace_path: str | Path,
    golden_path: str | Path | None = None,
    *,
    update_golden: bool = False,
    check_invariants: bool = True,
    base_stamp: int | None = None,
    max_observer_lead: int = 2,
) -> tuple[bool, dict[str, Any]]:
    """Load trace JSON from disk, check invariants, and compare against golden JSON."""
    trace = load_trace(trace_path)
    resolved_golden = (
        Path(golden_path)
        if golden_path is not None
        else GOLDEN_DIR / f"{trace['scenario']}.json"
    )
    return verify_trace_data(
        trace,
        resolved_golden,
        update_golden=update_golden,
        check_invariants=check_invariants,
        base_stamp=base_stamp,
        max_observer_lead=max_observer_lead,
    )


def build_arg_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description="Verify captured harness trace under virtual clock against golden action/relative-stamp JSON."
    )
    p.add_argument("--trace", required=True, help="Path to captured trace JSON file.")
    p.add_argument(
        "--golden",
        help="Path to golden JSON file (defaults to crates/eve-server/tests/golden/<scenario>.json).",
    )
    p.add_argument(
        "--scenario",
        help="Override golden filename stem under crates/eve-server/tests/golden/<scenario>.json.",
    )
    p.add_argument(
        "--update-golden",
        action="store_true",
        help="Write/overwrite the golden JSON file (also enabled by UPDATE_GOLDEN=1).",
    )
    p.add_argument(
        "--no-invariants",
        action="store_true",
        help="Skip destiny/clock invariant checks and only compare golden sequences.",
    )
    p.add_argument(
        "--base-stamp",
        type=int,
        default=None,
        help="Explicit base stamp for relative-stamp computation.",
    )
    p.add_argument(
        "--json",
        action="store_true",
        help="Emit machine-readable JSON output.",
    )
    return p


def main(argv: Sequence[str] | None = None) -> int:
    args = build_arg_parser().parse_args(argv)
    update_golden = args.update_golden or os.environ.get("UPDATE_GOLDEN") == "1"
    golden_path: Path | None = None
    if args.golden:
        golden_path = Path(args.golden)
    elif args.scenario:
        golden_path = GOLDEN_DIR / f"{args.scenario}.json"

    try:
        passed, result = verify_trace_file(
            args.trace,
            golden_path,
            update_golden=update_golden,
            check_invariants=not args.no_invariants,
            base_stamp=args.base_stamp,
        )
    except (OSError, ValueError, KeyError) as exc:
        print(f"replay verification error: {exc}", file=sys.stderr)
        return 2

    if args.json:
        print(json.dumps(result, indent=2))
    else:
        status = "PASS" if passed else "FAIL"
        print(f"[{status}] scenario={result['scenario']} golden={result['golden_path']}")
        if result["invariant_violations"]:
            print("Invariant violations:")
            for v in result["invariant_violations"]:
                print(f"  - {v}")
        print(result["golden_message"])

    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
