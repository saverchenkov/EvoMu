#!/usr/bin/env python3
"""Repository release-hygiene guard (GH #1077 §F).

Scans all git-tracked files under ``--root`` and fails with exit code 1 if any
client assets, compiled artifacts, databases, secrets/private keys, large
base64 payloads, review artifacts, or hardcoded developer user paths are found.

Standalone usage:
    python3 tools/release_guard.py [--root <dir>] [--json]
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable

FORBIDDEN_EXTENSIONS: frozenset[str] = frozenset(
    {
        ".pyj",
        ".pyc",
        ".stuff",
        ".ccp",
        ".dll",
        ".exe",
        ".pyd",
        ".sqlite",
        ".sqlite3",
        ".db",
        ".pem",
        ".key",
        ".p12",
        ".pfx",
    }
)

FORBIDDEN_DIR_NAMES: frozenset[str] = frozenset({".evomu-reviews"})

# Explicit allowlist for synthetic test DB fixtures (none tracked today).
ALLOWLIST_FORBIDDEN_EXT_PATHS: frozenset[str] = frozenset()

# Narrow allowlist for documentation examples, supervisor-owned scripts, and
# synthetic filename-sanitization unit tests. Note that these files are still
# scanned for forbidden extensions, private keys, and large base64 payloads.
ALLOWLIST_USER_PATH_PREFIXES: tuple[str, ...] = (".agents/",)
ALLOWLIST_USER_PATH_FILES: frozenset[str] = frozenset(
    {
        "AGENTS.md",
        "docs/SUPERVISOR_CODER_ARCHITECTURE.md",
        "crates/eve-server/src/bug_reporting_tests.rs",
    }
)

ALLOW_MARKER = "release-guard: allow"

PRIVATE_KEY_RE = re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----")

# Matches large base64 payloads (>= 512 chars) while rejecting pure hex strings
# (such as wire_packet_hex in ccp_golden_vectors.json) by requiring at least one
# non-hex base64 character ([G-Zg-z+/=]) or an explicit data:...;base64, prefix.
DATA_URI_BASE64_RE = re.compile(
    r"data:[A-Za-z0-9.+/-]+;base64,[A-Za-z0-9+/]{256,}={0,2}"
)
BASE64_TOKEN_RE = re.compile(r"(?<![A-Za-z0-9+/])[A-Za-z0-9+/]{512,}={0,2}(?![A-Za-z0-9+/=])")
NON_HEX_B64_RE = re.compile(r"[G-Zg-z+/=]")

# Root-anchored developer home paths; avoids false positives on relative client
# asset paths such as "character_sheet/home/details.pyj".
UNIX_USER_PATH_RE = re.compile(r"(?<![A-Za-z0-9_.\-/])/(?:Users|home)/[A-Za-z0-9_.-]+")
WIN_USER_PATH_RE = re.compile(r"(?<![A-Za-z0-9_])[A-Za-z]:\\+Users\\+[A-Za-z0-9_.-]+")

# Non-test production Rust code must use fallible ProtoBuilder methods instead of panicking `.must_*`.
PROTO_MUST_RE = re.compile(r"\.must_(?:to_any|encode|encode_into|to_dynamic)\s*\(")
CFG_TEST_ATTR_RE = re.compile(r"#\[\s*cfg\([^)]*\btest\b[^)]*\)\s*\]")
RAW_STR_PREFIX_RE = re.compile(r'(?:br|cr|r)(#*)(")')

# Skip content scans on binary extensions that aren't already forbidden (e.g. PNG).
BINARY_CONTENT_SKIP_EXTS: frozenset[str] = frozenset(
    {
        ".png",
        ".jpg",
        ".jpeg",
        ".gif",
        ".webp",
        ".ico",
        ".woff",
        ".woff2",
        ".ttf",
        ".otf",
        ".zip",
        ".gz",
        ".tar",
        ".xz",
        ".pdf",
    }
)


@dataclass(frozen=True)
class Violation:
    rule: str
    path: str
    line: int | None
    detail: str


def git_tracked_files(root: Path) -> list[str]:
    """Returns POSIX-normalized relative paths of all files tracked by git."""
    proc = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        stderr = proc.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"git ls-files failed under {root}: {stderr}")
    raw = proc.stdout
    if not raw:
        return []
    entries = [entry.decode("utf-8", errors="surrogateescape") for entry in raw.split(b"\x00") if entry]
    return sorted(entries)


def _is_user_path_allowlisted(rel_path: str) -> bool:
    if rel_path in ALLOWLIST_USER_PATH_FILES:
        return True
    return any(rel_path.startswith(prefix) for prefix in ALLOWLIST_USER_PATH_PREFIXES)


def _find_large_base64(line: str) -> str | None:
    m = DATA_URI_BASE64_RE.search(line)
    if m:
        return f"data-URI base64 payload ({len(m.group(0))} chars)"
    for m in BASE64_TOKEN_RE.finditer(line):
        token = m.group(0)
        if NON_HEX_B64_RE.search(token):
            return f"large base64 payload ({len(token)} chars)"
    return None


def _is_prod_rust_path(norm_path: str) -> bool:
    if not (norm_path.startswith("crates/") and norm_path.endswith(".rs")):
        return False
    if "/tests/" in norm_path or norm_path.endswith("_tests.rs") or norm_path.endswith("/tests.rs"):
        return False
    return True


def _iter_prod_rust_lines(text: str) -> Iterable[tuple[int, str]]:
    """Yields (1-based lineno, comment-and-string-stripped code) outside #[cfg(test)] items."""
    brace_depth = 0
    block_comment_depth = 0
    in_string = False
    raw_hashes: int | None = None
    cfg_test_pending = False
    cfg_test_target_depth = 0
    cfg_test_active_depth: int | None = None

    for lineno, line in enumerate(text.splitlines(), start=1):
        out: list[str] = []
        i = 0
        n = len(line)
        line_in_cfg_test = cfg_test_pending or cfg_test_active_depth is not None
        while i < n:
            ch = line[i]
            nxt = line[i + 1] if i + 1 < n else ""
            if block_comment_depth > 0:
                if ch == "/" and nxt == "*":
                    block_comment_depth += 1
                    i += 2
                elif ch == "*" and nxt == "/":
                    block_comment_depth -= 1
                    i += 2
                else:
                    i += 1
                continue
            if raw_hashes is not None:
                if ch == '"' and line[i + 1 : i + 1 + raw_hashes] == "#" * raw_hashes:
                    i += 1 + raw_hashes
                    raw_hashes = None
                    out.append('"')
                else:
                    i += 1
                continue
            if in_string:
                if ch == "\\" and i + 1 < n:
                    i += 2
                elif ch == '"':
                    in_string = False
                    i += 1
                    out.append('"')
                else:
                    i += 1
                continue
            if ch == "/" and nxt == "/":
                break
            if ch == "/" and nxt == "*":
                block_comment_depth += 1
                i += 2
                continue
            if ch in ("r", "b", "c"):
                raw_match = RAW_STR_PREFIX_RE.match(line[i:])
                if raw_match:
                    raw_hashes = len(raw_match.group(1))
                    i += len(raw_match.group(0))
                    out.append('"')
                    continue
            if ch == '"':
                in_string = True
                i += 1
                out.append('"')
                continue
            if ch == "'":
                if i + 1 < n and line[i + 1] == "\\":
                    j = i + 2
                    if j < n:
                        j += 1
                    while j < n and line[j] != "'":
                        j += 1
                    i = min(j + 1, n)
                    out.append("''")
                    continue
                if i + 2 < n and line[i + 2] == "'":
                    i += 3
                    out.append("''")
                    continue
            if ch == "{":
                brace_depth += 1
                if cfg_test_pending and brace_depth > cfg_test_target_depth:
                    cfg_test_active_depth = cfg_test_target_depth
                    cfg_test_pending = False
                    line_in_cfg_test = True
            elif ch == "}":
                brace_depth = max(0, brace_depth - 1)
                if cfg_test_active_depth is not None and brace_depth <= cfg_test_active_depth:
                    cfg_test_active_depth = None
                    line_in_cfg_test = True
            elif ch == ";":
                if cfg_test_pending and brace_depth <= cfg_test_target_depth:
                    cfg_test_pending = False
                    line_in_cfg_test = True
            out.append(ch)
            i += 1

        code = "".join(out)
        if not line_in_cfg_test and cfg_test_active_depth is None and CFG_TEST_ATTR_RE.search(code):
            cfg_test_pending = True
            cfg_test_target_depth = brace_depth
            line_in_cfg_test = True
        if not line_in_cfg_test:
            yield lineno, code


def scan_tracked_file(root: Path, rel_path: str) -> list[Violation]:
    """Scans a single tracked file for path-level and content-level violations."""
    violations: list[Violation] = []
    norm_path = rel_path.replace("\\", "/")
    parts = norm_path.split("/")

    for dir_name in FORBIDDEN_DIR_NAMES:
        if dir_name in parts[:-1] or parts[-1] == dir_name:
            violations.append(
                Violation(
                    rule="forbidden_review_dir",
                    path=norm_path,
                    line=None,
                    detail=f"tracked file inside forbidden directory '{dir_name}'",
                )
            )

    ext = os.path.splitext(norm_path)[1].lower()
    if ext in FORBIDDEN_EXTENSIONS and norm_path not in ALLOWLIST_FORBIDDEN_EXT_PATHS:
        violations.append(
            Violation(
                rule="forbidden_extension",
                path=norm_path,
                line=None,
                detail=f"tracked file has forbidden extension '{ext}'",
            )
        )

    if ext in BINARY_CONTENT_SKIP_EXTS:
        return violations

    full_path = root / rel_path
    if not full_path.is_file():
        return violations

    try:
        raw_bytes = full_path.read_bytes()
    except OSError as exc:
        violations.append(
            Violation(
                rule="unreadable_file",
                path=norm_path,
                line=None,
                detail=f"failed to read tracked file: {exc}",
            )
        )
        return violations

    # If the file looks binary (contains NUL bytes), only scan ASCII lines for keys/paths
    # rather than skipping; private keys or hardcoded paths in blobs should still be caught.
    text = raw_bytes.decode("utf-8", errors="replace")
    lines = text.splitlines()
    check_user_paths = not _is_user_path_allowlisted(norm_path)

    for lineno, line in enumerate(lines, start=1):
        key_match = PRIVATE_KEY_RE.search(line)
        if key_match:
            violations.append(
                Violation(
                    rule="private_key_header",
                    path=norm_path,
                    line=lineno,
                    detail=f"found private key header '{key_match.group(0)}'",
                )
            )

        if ALLOW_MARKER in line:
            continue

        b64_detail = _find_large_base64(line)
        if b64_detail:
            violations.append(
                Violation(
                    rule="large_base64_payload",
                    path=norm_path,
                    line=lineno,
                    detail=b64_detail,
                )
            )

        if check_user_paths:
            unix_match = UNIX_USER_PATH_RE.search(line)
            if unix_match:
                violations.append(
                    Violation(
                        rule="hardcoded_user_path",
                        path=norm_path,
                        line=lineno,
                        detail=f"found hardcoded Unix user path '{unix_match.group(0)}'",
                    )
                )
                continue
            win_match = WIN_USER_PATH_RE.search(line)
            if win_match:
                violations.append(
                    Violation(
                        rule="hardcoded_user_path",
                        path=norm_path,
                        line=lineno,
                        detail=f"found hardcoded Windows user path '{win_match.group(0)}'",
                    )
                )

    if _is_prod_rust_path(norm_path) and ".must_" in text:
        for lineno, code in _iter_prod_rust_lines(text):
            raw_line = lines[lineno - 1]
            if ALLOW_MARKER in raw_line:
                continue
            must_match = PROTO_MUST_RE.search(code)
            if must_match:
                violations.append(
                    Violation(
                        rule="proto_must_in_prod",
                        path=norm_path,
                        line=lineno,
                        detail=f"production code calls panicking ProtoBuilder method '{must_match.group(0)}...'",
                    )
                )

    return violations


def scan_repo(root: Path) -> tuple[int, list[Violation]]:
    """Scans all git-tracked files under ``root`` and returns (file_count, violations)."""
    tracked = git_tracked_files(root)
    violations: list[Violation] = []
    for rel_path in tracked:
        violations.extend(scan_tracked_file(root, rel_path))
    return len(tracked), violations


def format_violations(violations: Iterable[Violation]) -> str:
    lines: list[str] = []
    for v in violations:
        loc = f"{v.path}:{v.line}" if v.line is not None else v.path
        lines.append(f"[ERROR] {v.rule}: {loc} — {v.detail}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Verify repository release hygiene.")
    parser.add_argument(
        "--root",
        type=Path,
        default=Path.cwd(),
        help="Repository root directory to scan (defaults to current working directory).",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit machine-readable JSON output.",
    )
    args = parser.parse_args(argv)
    root = args.root.resolve()

    try:
        file_count, violations = scan_repo(root)
    except RuntimeError as exc:
        if args.json:
            print(json.dumps({"ok": False, "error": str(exc)}, indent=2))
        else:
            print(f"[FATAL] {exc}", file=sys.stderr)
        return 2

    ok = len(violations) == 0
    if args.json:
        print(
            json.dumps(
                {
                    "ok": ok,
                    "root": str(root),
                    "scanned_files": file_count,
                    "violations": [asdict(v) for v in violations],
                },
                indent=2,
            )
        )
    else:
        if ok:
            print(f"release_guard: CLEAN ({file_count} tracked files scanned under {root})")
        else:
            print(format_violations(violations), file=sys.stderr)
            print(
                f"release_guard: FAILED ({len(violations)} violation(s) across {file_count} tracked files)",
                file=sys.stderr,
            )

    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
