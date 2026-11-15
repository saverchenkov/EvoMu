#!/usr/bin/env python3
"""Unit tests for unified SDE + Quasar descriptor manifest verification."""

from __future__ import annotations

import hashlib
import json
import sqlite3
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
QUASAR_IMPORT = HERE.parent / "quasar-import"
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(QUASAR_IMPORT))

from extract_quasar_descriptors import verify_descriptors_db  # noqa: E402
from import_sde import (  # noqa: E402
    compute_sde_resources_sha256,
    resolve_quasar_out_path,
    verify_sde_db,
)


def _create_sample_sde(db_path: Path, manifest_path: Path, build: str = "3698585") -> None:
    resources = [
        {
            "res_path": "res:/staticdata/types.static",
            "cdn_relative_path": "ab/abc",
            "md5": "d41d8cd98f00b204e9800998ecf8427e",
            "byte_size": 128,
            "format": "fsd",
            "record_count": 2,
        }
    ]
    res_sha256 = compute_sde_resources_sha256(resources)

    conn = sqlite3.connect(db_path)
    conn.execute("CREATE TABLE sde_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
    conn.execute("CREATE TABLE _evomu_sde_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
    conn.execute("CREATE TABLE sde_types (type_id INTEGER PRIMARY KEY)")
    conn.executemany(
        "INSERT INTO sde_meta (key, value) VALUES (?, ?)",
        [
            ("schema_version", "1"),
            ("client_build", build),
            ("client_release", "V24.02"),
        ],
    )
    conn.executemany(
        "INSERT INTO _evomu_sde_meta (key, value) VALUES (?, ?)",
        [
            ("schema_version", "1"),
            ("client_build", build),
            ("client_release", "V24.02"),
            ("resource_count", str(len(resources))),
            ("combined_resource_sha256", res_sha256),
        ],
    )
    conn.executemany("INSERT INTO sde_types (type_id) VALUES (?)", [(587,), (670,)])
    conn.commit()
    conn.close()

    manifest = {
        "schema_version": 1,
        "client_build": build,
        "client_release": "V24.02",
        "resource_count": len(resources),
        "combined_resource_sha256": res_sha256,
        "table_counts": {"_evomu_sde_meta": 5, "sde_meta": 3, "sde_types": 2},
        "resources": resources,
    }
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")


def _create_sample_quasar(db_path: Path, manifest_path: Path) -> None:
    pyj_path = "public/test.pb2"
    payload = b"\n\x0btest.proto"
    sha256 = hashlib.sha256(payload).hexdigest()
    md5 = hashlib.md5(payload).hexdigest()
    path_bytes = pyj_path.encode("utf-8")
    header = struct.pack("<I", len(path_bytes)) + path_bytes + struct.pack("<I", len(payload))
    combined_sha256 = hashlib.sha256(header + payload).hexdigest()
    combined_md5 = hashlib.md5(header + payload).hexdigest()

    conn = sqlite3.connect(db_path)
    conn.execute(
        """
        CREATE TABLE quasar_file_descriptors (
            pyj_path TEXT PRIMARY KEY,
            domain TEXT NOT NULL,
            sha256 TEXT NOT NULL,
            md5 TEXT NOT NULL,
            serialized_pb BLOB NOT NULL
        )
        """
    )
    conn.execute(
        "INSERT INTO quasar_file_descriptors (pyj_path, domain, sha256, md5, serialized_pb) "
        "VALUES (?, ?, ?, ?, ?)",
        (pyj_path, "public", sha256, md5, payload),
    )
    conn.commit()
    conn.close()

    manifest = {
        "schema_version": 1,
        "domain_count": 1,
        "descriptor_file_count": 1,
        "combined_serialized_pb_sha256": combined_sha256,
        "combined_serialized_pb_md5": combined_md5,
        "per_domain_file_counts": {"public": 1},
    }
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")


class TestStaticImportManifestVerification(unittest.TestCase):
    def test_verify_sde_db_passes_and_catches_stale_build_or_counts(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_dir = Path(tmp)
            db_path = tmp_dir / "evomu_sde.sqlite"
            manifest_path = tmp_dir / "sde_import_manifest.json"
            _create_sample_sde(db_path, manifest_path)

            verified = verify_sde_db(db_path, manifest_path)
            self.assertEqual(verified["client_build"], "3698585")

            # Missing DB raises FileNotFoundError
            with self.assertRaises(FileNotFoundError):
                verify_sde_db(tmp_dir / "missing.sqlite", manifest_path)

            # Stale client build fails
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["client_build"] = "9999999"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "client_build mismatch"):
                verify_sde_db(db_path, manifest_path)

            # Resource SHA-256 mismatch fails
            manifest["client_build"] = "3698585"
            manifest["combined_resource_sha256"] = "0" * 64
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "combined_resource_sha256 mismatch"):
                verify_sde_db(db_path, manifest_path)

            # Table count mismatch fails
            manifest.pop("combined_resource_sha256", None)
            manifest["table_counts"]["sde_types"] = 99
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "row count mismatch"):
                verify_sde_db(db_path, manifest_path)

    def test_verify_descriptors_db_passes_and_catches_checksum_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_dir = Path(tmp)
            db_path = tmp_dir / "quasar_descriptors.sqlite"
            manifest_path = tmp_dir / "quasar_descriptor_manifest.json"
            _create_sample_quasar(db_path, manifest_path)

            verified = verify_descriptors_db(db_path, manifest_path)
            self.assertEqual(verified["descriptor_file_count"], 1)

            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["combined_serialized_pb_sha256"] = "0" * 64
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "combined SHA-256 mismatch"):
                verify_descriptors_db(db_path, manifest_path)

    def test_cli_verify_only_and_help(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_dir = Path(tmp)
            sde_db = tmp_dir / "evomu_sde.sqlite"
            sde_manifest = tmp_dir / "sde_import_manifest.json"
            quasar_db = tmp_dir / "quasar_descriptors.sqlite"
            quasar_manifest = tmp_dir / "quasar_descriptor_manifest.json"
            _create_sample_sde(sde_db, sde_manifest)
            _create_sample_quasar(quasar_db, quasar_manifest)

            self.assertEqual(resolve_quasar_out_path(sde_db, None), quasar_db)

            res = subprocess.run(
                [
                    sys.executable,
                    str(HERE / "import_sde.py"),
                    "--out-db",
                    str(sde_db),
                    "--manifest",
                    str(sde_manifest),
                    "--quasar-manifest",
                    str(quasar_manifest),
                    "--verify-only",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(res.returncode, 0, msg=res.stderr or res.stdout)
            self.assertIn("[sde-verify]", res.stdout)
            self.assertIn("[quasar-verify]", res.stdout)

        for script in [
            HERE / "import_sde.py",
            QUASAR_IMPORT / "extract_quasar_descriptors.py",
        ]:
            help_res = subprocess.run(
                [sys.executable, str(script), "--help"],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(help_res.returncode, 0, msg=help_res.stderr)


if __name__ == "__main__":
    unittest.main()
