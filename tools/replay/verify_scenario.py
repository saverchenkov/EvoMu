#!/usr/bin/env python3
"""Scenario entrypoint for replaying captured §D destiny journey traces under VirtualClock."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import sys
from typing import Sequence

THIS_DIR = Path(__file__).resolve().parent
REPO_ROOT = THIS_DIR.parents[1]
GOLDEN_DIR = REPO_ROOT / "crates" / "eve-server" / "tests" / "golden"

if str(THIS_DIR.parent) not in sys.path:
    sys.path.insert(0, str(THIS_DIR.parent))

from replay.verify_trace import verify_trace_file  # noqa: E402

CANONICAL_SCENARIOS: dict[str, str] = {
    "undock_approach_orbit_stop": "Undock -> approach -> orbit -> stop",
    "warp_to_belt_with_watcher": "Warp to belt with a watcher",
    "missile_volley_turning": "Missile volley while turning",
    "turret_kill_wreck_pod": "Turret kill -> wreck -> pod",
    "gate_jump_tidi": "Gate jump under TiDi",
}


def resolve_scenario_paths(
    scenario: str,
    *,
    trace_override: str | None = None,
    golden_override: str | None = None,
) -> tuple[Path, Path]:
    trace_path = (
        Path(trace_override)
        if trace_override
        else GOLDEN_DIR / "traces" / f"{scenario}.trace.json"
    )
    golden_path = (
        Path(golden_override)
        if golden_override
        else GOLDEN_DIR / f"{scenario}.json"
    )
    return trace_path, golden_path


def main(argv: Sequence[str] | None = None) -> int:
    p = argparse.ArgumentParser(
        description="Verify a named destiny/motion journey trace against its committed golden file."
    )
    p.add_argument(
        "scenario",
        nargs="?",
        help="Scenario name (see --list for canonical §D journeys).",
    )
    p.add_argument("--trace", help="Explicit path to captured trace JSON.")
    p.add_argument("--golden", help="Explicit path to golden JSON.")
    p.add_argument("--list", action="store_true", help="List canonical §D scenarios and exit.")
    p.add_argument(
        "--update-golden",
        action="store_true",
        help="Write/overwrite golden JSON (also enabled by UPDATE_GOLDEN=1).",
    )
    p.add_argument("--json", action="store_true", help="Emit JSON output.")
    args = p.parse_args(argv)

    if args.list:
        for name, desc in CANONICAL_SCENARIOS.items():
            print(f"{name}: {desc}")
        return 0

    if not args.scenario:
        p.error("scenario name is required unless --list is passed")

    trace_path, golden_path = resolve_scenario_paths(
        args.scenario, trace_override=args.trace, golden_override=args.golden
    )
    update_golden = args.update_golden or os.environ.get("UPDATE_GOLDEN") == "1"

    try:
        passed, result = verify_trace_file(
            trace_path, golden_path, update_golden=update_golden
        )
    except (OSError, ValueError, KeyError) as exc:
        print(f"scenario verification error: {exc}", file=sys.stderr)
        return 2

    if args.json:
        import json

        print(json.dumps(result, indent=2))
    else:
        status = "PASS" if passed else "FAIL"
        print(f"[{status}] {args.scenario}: {result['golden_message']}")
        for v in result["invariant_violations"]:
            print(f"  - {v}")

    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
