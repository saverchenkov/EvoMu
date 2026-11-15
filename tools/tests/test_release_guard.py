"""Unit tests for tools/release_guard.py (GH #1077 §F)."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import release_guard  # noqa: E402


def _init_git_repo(root: Path) -> None:
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.name", "Test"], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.email", "test@example.com"], check=True)


def _track_file(root: Path, rel: str, content: bytes | str) -> Path:
    target = root / rel
    target.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(content, bytes):
        target.write_bytes(content)
    else:
        target.write_text(content, encoding="utf-8")
    subprocess.run(["git", "-C", str(root), "add", "-f", rel], check=True)
    return target


class ReleaseGuardTests(unittest.TestCase):
    def test_clean_tree_passes_and_ignores_benign_hex_or_relative_home(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _init_git_repo(root)
            _track_file(
                root,
                "src/lib.rs",
                "// clean file\npub fn answer() -> i32 { 42 }\n",
            )
            # Client relative asset path containing "/home/" must not false-positive.
            _track_file(
                root,
                "docs/paths.json",
                '{"path": "expertSystems/client/ui/character_sheet/home/details.pyj"}\n',
            )
            # Long pure-hex packet vector must not false-positive as base64.
            _track_file(
                root,
                "tests/vectors.json",
                json.dumps({"wire_packet_hex": "deadbeef" * 90}) + "\n",
            )
            # Untracked forbidden artifacts must not trigger because git ls-files is scanned.
            (root / "scratch.sqlite").write_bytes(b"SQLite format 3\x00")
            (root / ".evomu-reviews").mkdir()
            (root / ".evomu-reviews" / "report.md").write_text("review", encoding="utf-8")

            count, violations = release_guard.scan_repo(root)
            self.assertEqual(count, 3)
            self.assertEqual(violations, [])
            self.assertEqual(release_guard.main(["--root", str(root)]), 0)

    def test_catches_every_forbidden_extension(self) -> None:
        forbidden_exts = sorted(release_guard.FORBIDDEN_EXTENSIONS)
        for ext in forbidden_exts:
            with self.subTest(ext=ext), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                _init_git_repo(root)
                _track_file(root, f"artifacts/sample{ext}", b"\x00\x01\x02payload")

                _, violations = release_guard.scan_repo(root)
                rules = {v.rule for v in violations}
                self.assertIn("forbidden_extension", rules, f"missed extension {ext}")
                self.assertEqual(release_guard.main(["--root", str(root)]), 1)

    def test_catches_evomu_reviews_tracked_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _init_git_repo(root)
            _track_file(root, ".evomu-reviews/123/review.md", "# review\n")

            _, violations = release_guard.scan_repo(root)
            self.assertEqual([v.rule for v in violations], ["forbidden_review_dir"])
            self.assertEqual(release_guard.main(["--root", str(root)]), 1)

    def test_catches_private_key_headers(self) -> None:
        headers = [
            "-----BEGIN " + "PRIVATE KEY-----",
            "-----BEGIN RSA " + "PRIVATE KEY-----",
            "-----BEGIN EC " + "PRIVATE KEY-----",
            "-----BEGIN OPENSSH " + "PRIVATE KEY-----",
        ]
        for header in headers:
            with self.subTest(header=header), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                _init_git_repo(root)
                _track_file(root, "config/secret.txt", f"key:\n{header}\nabc\n")

                _, violations = release_guard.scan_repo(root)
                self.assertTrue(any(v.rule == "private_key_header" for v in violations))
                self.assertEqual(release_guard.main(["--root", str(root)]), 1)

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _init_git_repo(root)
            _track_file(
                root,
                "src/key_allow.rs",
                "-----BEGIN RSA " + "PRIVATE KEY----- // release-guard: allow test\n",
            )
            _, violations = release_guard.scan_repo(root)
            self.assertTrue(any(v.rule == "private_key_header" for v in violations))

    def test_catches_large_base64_payloads_and_data_uris(self) -> None:
        # Raw 562-char base64 payload containing non-hex chars (G-Z, +, /, =)
        raw_payload = ("QWxsWW91ckJhc2VBcmVCZWxvbmc0" * 20) + "=="
        # Data-URI base64 payload (>= 256 chars after prefix)
        data_uri = "data:application/octet-stream;base64," + ("YWJjZGVmZ2hpamts" * 20) + "=="

        for label, body in [("raw", f'payload = "{raw_payload}"\n'), ("data_uri", f'patch = "{data_uri}"\n')]:
            with self.subTest(kind=label), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                _init_git_repo(root)
                _track_file(root, "patch.txt", body)

                _, violations = release_guard.scan_repo(root)
                self.assertTrue(any(v.rule == "large_base64_payload" for v in violations))
                self.assertEqual(release_guard.main(["--root", str(root)]), 1)

    def test_catches_hardcoded_user_paths_and_respects_allow_marker(self) -> None:
        samples = [
            ("macos", 'let p = "' + "/Users/" + 'alice/repo/data.sqlite";\n'),
            ("linux", 'DATA="' + "/home/" + 'bob/cache"\n'),
            ("windows_raw", 'path = r"' + "C:\\Users\\" + 'charlie\\Desktop"\n'),
            ("windows_escaped", 'path = "' + "C:\\\\Users\\\\" + 'charlie\\\\file.txt";\n'),
        ]
        for label, line in samples:
            with self.subTest(kind=label), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                _init_git_repo(root)
                _track_file(root, "src/config.rs", line)

                _, violations = release_guard.scan_repo(root)
                self.assertTrue(any(v.rule == "hardcoded_user_path" for v in violations))
                self.assertEqual(release_guard.main(["--root", str(root)]), 1)

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _init_git_repo(root)
            _track_file(
                root,
                "src/example.rs",
                'let p = "' + "/Users/" + 'alice/example"; // release-guard: allow doc fixture\n',
            )
            _, violations = release_guard.scan_repo(root)
            self.assertEqual(violations, [])

    def test_json_output_and_invalid_root_exit_code(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            non_git = Path(tmp) / "not_a_repo"
            non_git.mkdir()
            self.assertEqual(release_guard.main(["--root", str(non_git)]), 2)


if __name__ == "__main__":
    unittest.main()
