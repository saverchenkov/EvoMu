#!/usr/bin/env python3
# parity: plan:T-5
"""Workspace coverage (`cargo-llvm-cov`) and mutation (`cargo-mutants`) gate for T-5.

Verifies that:
1. `cargo llvm-cov` runs cleanly across the workspace (or specified packages) and produces a valid JSON coverage summary.
2. `cargo mutants` runs cleanly against a git diff (`--in-diff`, defaulting to `git diff HEAD`) or target crate and achieves
   a strict mutation score `caught / (caught + missed + timeout) >= 0.85` (85%), with timeouts excluded from the numerator.

Recorded Track A (`T-1`, `T-3`, `T-5`) verification run:
- `mutants[eve-destiny]: caught=26, missed=0, timeout=0, unviable=2`
- `mutants[eve-machonet]: caught=17, missed=2, timeout=0, unviable=31`
- `mutants[eve-proto]: caught=136, missed=23, timeout=7, unviable=5`
- `mutants[eve-server]: caught=7, missed=0, timeout=0, unviable=0`
- `mutants[eve-testkit]: caught=20, missed=0, timeout=0, unviable=0`
- `mutants summary: caught=206, missed=25, timeout=7, unviable=38, score=86.55%`
- `{"status": "OK", "packages": ["workspace"], "line_coverage_pct": 70.29, "mutants_caught": 206, "mutants_missed": 25, "mutants_timeout": 7, "mutants_unviable": 38, "mutation_score_pct": 86.55}`
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def run_checked(cmd: list[str], cwd: Path) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(
        cmd,
        cwd=str(cwd),
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        sys.stderr.write(f"Command failed ({proc.returncode}): {' '.join(cmd)}\n")
        sys.stderr.write(proc.stdout)
        sys.stderr.write(proc.stderr)
        raise SystemExit(proc.returncode)
    return proc


def check_coverage(repo_root: Path, packages: list[str], min_line_pct: float) -> float:
    cmd = ["cargo", "llvm-cov", "--json", "--summary-only"]
    if packages:
        for pkg in packages:
            cmd.extend(["-p", pkg])
    else:
        cmd.append("--workspace")
    proc = run_checked(cmd, repo_root)
    data = json.loads(proc.stdout)
    totals = data["data"][0]["totals"]["lines"]
    pct = float(totals["percent"])
    if pct < min_line_pct:
        sys.stderr.write(
            f"Coverage gate failed: line coverage {pct:.2f}% < {min_line_pct:.2f}%\n"
        )
        raise SystemExit(1)
    return pct


def discover_diff_packages(diff_text: str) -> list[str]:
    pkgs: list[str] = []
    for line in diff_text.splitlines():
        if line.startswith("+++ b/crates/"):
            parts = line.removeprefix("+++ b/crates/").split("/")
            if len(parts) >= 2 and parts[1] == "src":
                pkg = parts[0]
                if pkg not in pkgs:
                    pkgs.append(pkg)
    return pkgs


def check_mutants(
    repo_root: Path,
    packages: list[str],
    diff_file: Path | None,
    min_mutation_score: float,
) -> tuple[int, int, int, int, float]:
    with tempfile.TemporaryDirectory(prefix="evomu-mutants-") as tmp:
        effective_diff = diff_file
        diff_text = ""
        if effective_diff is None:
            diff_proc = subprocess.run(
                ["git", "diff", "HEAD", "--", "crates/"],
                cwd=str(repo_root),
                text=True,
                capture_output=True,
                check=False,
            )
            if diff_proc.returncode == 0 and diff_proc.stdout.strip():
                diff_text = diff_proc.stdout
                auto_diff = Path(tmp) / "head.diff"
                auto_diff.write_text(diff_text, encoding="utf-8")
                effective_diff = auto_diff
        elif effective_diff.exists():
            diff_text = effective_diff.read_text(encoding="utf-8")

        target_packages = packages or discover_diff_packages(diff_text) or ["eve-testkit"]
        total_caught = 0
        total_missed = 0
        total_timeout = 0
        total_unviable = 0

        env = os.environ.copy()
        env["PROPTEST_CASES"] = env.get("PROPTEST_CASES", "64")

        for idx, pkg in enumerate(target_packages):
            out_dir = Path(tmp) / f"out-{idx}-{pkg}"
            cmd = [
                "cargo",
                "mutants",
                "-j",
                "4",
                "--no-shuffle",
                "--minimum-test-timeout",
                "5",
                "-p",
                pkg,
                "-o",
                str(out_dir),
            ]
            if effective_diff is not None:
                cmd.extend(["--in-diff", str(effective_diff)])

            proc = subprocess.run(
                cmd,
                cwd=str(repo_root),
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            outcomes_path = out_dir / "mutants.out" / "outcomes.json"
            if not outcomes_path.exists():
                sys.stderr.write(
                    f"cargo mutants failed for {pkg} to produce {outcomes_path}:\n"
                )
                sys.stderr.write(proc.stdout)
                sys.stderr.write(proc.stderr)
                raise SystemExit(proc.returncode or 1)

            outcomes = json.loads(outcomes_path.read_text(encoding="utf-8"))
            caught = int(outcomes.get("caught", 0))
            missed = int(outcomes.get("missed", 0))
            timeout = int(outcomes.get("timeout", 0))
            unviable = int(outcomes.get("unviable", 0))
            total_caught += caught
            total_missed += missed
            total_timeout += timeout
            total_unviable += unviable
            print(
                f"mutants[{pkg}]: caught={caught}, missed={missed}, "
                f"timeout={timeout}, unviable={unviable}"
            )

        denom = total_caught + total_missed + total_timeout
        score = 100.0 if denom == 0 else (100.0 * total_caught / denom)
        print(
            f"mutants summary: caught={total_caught}, missed={total_missed}, "
            f"timeout={total_timeout}, unviable={total_unviable}, score={score:.2f}%"
        )
        if score < min_mutation_score:
            sys.stderr.write(
                f"Mutation gate failed: score {score:.2f}% < {min_mutation_score:.2f}%\n"
            )
            raise SystemExit(1)
        return total_caught, total_missed, total_timeout, total_unviable, score


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--package",
        "-p",
        action="append",
        default=[],
        help="Crate package(s) to scope coverage and mutation checks to (default: entire workspace).",
    )
    parser.add_argument(
        "--in-diff",
        type=Path,
        default=None,
        help="Optional unified diff file path for `cargo mutants --in-diff` (defaults to `git diff HEAD -- crates/` if non-empty).",
    )
    parser.add_argument(
        "--min-coverage",
        type=float,
        default=70.0,
        help="Minimum line coverage percentage (default: 70.0).",
    )
    parser.add_argument(
        "--min-mutation-score",
        type=float,
        default=85.0,
        help="Minimum mutation score percentage (default: 85.0).",
    )
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parents[2]
    packages = args.package

    line_pct = check_coverage(repo_root, packages, args.min_coverage)
    caught, missed, timeout, unviable, score = check_mutants(
        repo_root, packages, args.in_diff, args.min_mutation_score
    )
    print(
        json.dumps(
            {
                "status": "OK",
                "packages": packages or ["workspace"],
                "line_coverage_pct": round(line_pct, 2),
                "mutants_caught": caught,
                "mutants_missed": missed,
                "mutants_timeout": timeout,
                "mutants_unviable": unviable,
                "mutation_score_pct": round(score, 2),
            }
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
