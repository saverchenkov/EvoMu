#!/usr/bin/env python3
"""
Curate & normalize `.playtester/memory.json` coverage_frontier:
- Maps legacy/alias feature names onto canonical `initial_frontier` IDs
- Removes bloated orphan `persona=""` entries
- Caps custom non-canonical features per persona
- Decays stale `"buggy"` items -> `"untested"` (`needs_retest=True`) when the server
  commit/binary changes or when `--decay-stale-buggy` is specified
- Updates `last_server_git_head` and `last_server_bin_mtime`
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
import time
from pathlib import Path
from typing import Any, Dict, Optional

HARNESS_DIR = Path(__file__).resolve().parent
if str(HARNESS_DIR) not in sys.path:
    sys.path.insert(0, str(HARNESS_DIR))

from playtester import PlaytesterMemory, get_git_head


def _resolve_server_bin_mtime(worktree_dir: Path, server_bin: Optional[Path] = None) -> Optional[float]:
    if server_bin is not None:
        return float(server_bin.stat().st_mtime) if server_bin.exists() else None
    for rel in ("target/debug/eve-server", "target/release/eve-server"):
        cand = worktree_dir / rel
        if cand.exists():
            return float(cand.stat().st_mtime)
    return None


def curate_memory_file(
    memory_path: Path,
    personas_path: Optional[Path] = None,
    worktree_dir: Optional[Path] = None,
    server_bin: Optional[Path] = None,
    decay_stale_buggy: bool = False,
    dry_run: bool = False,
    create_backup: bool = True,
) -> Dict[str, Any]:
    memory_path = memory_path.resolve()
    state_dir = memory_path.parent
    wt = worktree_dir.resolve() if worktree_dir is not None else (
        state_dir.parent if state_dir.name == ".playtester" else HARNESS_DIR.parent.parent
    )
    git_head = get_git_head(wt)
    bin_mtime = _resolve_server_bin_mtime(wt, server_bin=server_bin)

    mem = PlaytesterMemory(state_dir, personas_path=personas_path)
    stats = mem.normalize_coverage_frontier(
        git_head=git_head or None,
        server_bin_mtime=bin_mtime,
        decay_stale_buggy=decay_stale_buggy,
        save=False,
    )
    stats["git_head"] = git_head
    stats["server_bin_mtime"] = bin_mtime
    stats["dry_run"] = dry_run

    # Count resulting statuses & personas for summary visibility
    by_status: Dict[str, int] = {}
    by_persona: Dict[str, int] = {}
    empty_persona = 0
    for item in mem.data.get("coverage_frontier") or []:
        st = str(item.get("status") or "untested")
        by_status[st] = by_status.get(st, 0) + 1
        pid = str(item.get("persona") or "")
        if not pid:
            empty_persona += 1
        else:
            by_persona[pid] = by_persona.get(pid, 0) + 1

    stats["status_breakdown"] = by_status
    stats["persona_breakdown"] = by_persona
    stats["empty_persona_count"] = empty_persona

    if not dry_run:
        if create_backup and memory_path.exists():
            bak = memory_path.with_name(f"{memory_path.name}.pre_curate.{int(time.time())}.bak")
            shutil.copy2(str(memory_path), str(bak))
            stats["backup_path"] = str(bak)
        mem.save()

    return stats


def main(argv: Optional[list] = None) -> int:
    default_wt = HARNESS_DIR.parent.parent
    p = argparse.ArgumentParser(description="Curate EvoMu playtester memory.json coverage frontier")
    p.add_argument(
        "--worktree",
        type=Path,
        default=default_wt,
        help="Worktree root containing .playtester/memory.json (default: repo root)",
    )
    p.add_argument(
        "--memory",
        type=Path,
        default=None,
        help="Explicit path to memory.json (default: <worktree>/.playtester/memory.json)",
    )
    p.add_argument(
        "--personas",
        type=Path,
        default=None,
        help="Optional override path to playtester_personas.json",
    )
    p.add_argument(
        "--server-bin",
        type=Path,
        default=None,
        help="Optional explicit path to eve-server binary for mtime tracking",
    )
    p.add_argument(
        "--decay-stale-buggy",
        action="store_true",
        help="Decay existing 'buggy' frontier items to 'untested' (needs_retest=True) during curation",
    )
    p.add_argument(
        "--dry-run",
        action="store_true",
        help="Compute curation stats without writing memory.json or backup",
    )
    p.add_argument(
        "--no-backup",
        action="store_true",
        help="Do not create a .bak copy of memory.json before writing",
    )
    args = p.parse_args(argv)

    wt = args.worktree.resolve()
    mem_path = args.memory.resolve() if args.memory is not None else (wt / ".playtester" / "memory.json")
    stats = curate_memory_file(
        memory_path=mem_path,
        personas_path=args.personas.resolve() if args.personas is not None else None,
        worktree_dir=wt,
        server_bin=args.server_bin.resolve() if args.server_bin is not None else None,
        decay_stale_buggy=args.decay_stale_buggy,
        dry_run=args.dry_run,
        create_backup=not args.no_backup,
    )
    print(json.dumps(stats, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
