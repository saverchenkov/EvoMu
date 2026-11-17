#!/usr/bin/env python3
"""Unit tests for unified SDE + Quasar descriptor manifest verification."""

from __future__ import annotations

import hashlib
import json
import pickle
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
from sde_extractors import extract_v24_achievement_career_daily_catalogs  # noqa: E402


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

    def test_extract_v24_achievement_career_daily_catalogs(self) -> None:
        main_pickle = pickle.dumps(
            {
                "labels": {
                    1001: {"FullPath": "UI/Achievements/Definitions/Categories", "label": "Combat_Name"},
                    2001: {"FullPath": "Character/CareerGoals/Enforcer/GroupNames", "label": "Combat"},
                    2002: {"FullPath": "Character/CareerGoals/Enforcer/GroupNames", "label": "Market"},
                    2010: {"FullPath": "Character/CareerGoals/Enforcer/Names", "label": "EnforcerDestroyNpc100"},
                    2011: {"FullPath": "Character/CareerGoals/Enforcer/Descriptions", "label": "EnforcerDestroyNpc100"},
                    2012: {"FullPath": "Character/CareerGoals/Enforcer/AuraHints", "label": "EnforcerDestroyNpc100"},
                    2013: {"FullPath": "Character/CareerGoals/Enforcer/AgencyLinks", "label": "EnforcerDestroyNpc100"},
                    2020: {"FullPath": "Character/CareerGoals/Enforcer/Names", "label": "EnforcerSpendMarketIsk1m"},
                    2021: {"FullPath": "Character/CareerGoals/Enforcer/Descriptions", "label": "EnforcerSpendMarketIsk1m"},
                    2022: {"FullPath": "Character/CareerGoals/Enforcer/AuraHints", "label": "EnforcerSpendMarketIsk1m"},
                    2030: {"FullPath": "Character/CareerGoals/Explorer/GroupNames", "label": "Scanning"},
                    2031: {"FullPath": "Character/CareerGoals/Explorer/Names", "label": "ExplorerScanSignature1"},
                    2032: {"FullPath": "Character/CareerGoals/Explorer/Descriptions", "label": "ExplorerScanSignature1"},
                    2033: {"FullPath": "Character/CareerGoals/Explorer/AuraHints", "label": "ExplorerScanSignature1"},
                    3001: {"FullPath": "UI/DailyGoals/Names", "label": "DailyCompletionBonus"},
                    3002: {"FullPath": "Notifications/DailyGoals", "label": "GoalCompleted"},
                }
            }
        )
        en_labels = {
            1001: "Combat",
            2001: "Combat",
            2002: "Market",
            2010: "Destroy 100 enemy ships",
            2011: "Desc 100",
            2012: "Aura 100",
            2013: "Agency Link",
            2020: "Spend 1M\xa0ISK on the Market",
            2021: "Desc 1M",
            2022: "Aura 1M",
            2030: "Scanning",
            2031: "Scan a Cosmic Signature to 100%",
            2032: "Desc Scan",
            2033: "Aura Scan",
            3001: "Daily Completion Bonus",
            3002: "Daily Goal Completed",
            4001: "Ach Name",
            4002: "Ach Desc",
            4003: "Ach Notice",
            5001: "Enforcer",
            5002: "Enforcer Desc",
            6001: "Scan Title",
            6002: "Scan Desc",
            6003: "Scan Prog",
            6004: "Scan Rew",
            6005: "Scan Tgt",
        }
        fsd_data = {
            "achievements": [
                {
                    "achievement_id": 60,
                    "name_id": 4001,
                    "description_id": 4002,
                    "notification_text_id": 4003,
                    "is_client_achievement": 1,
                    "is_enabled": 1,
                    "simple_conditions": [{"condition_index": 0, "statistic": 138, "target_value": 3}],
                }
            ],
            "achievement_events": [{"event_id": 138, "name": "testEvent", "description": "Test desc"}],
            "career_paths": [
                {"career_path_id": 6, "internal_name": "Enforcer", "name_id": 5001, "description_id": 5002}
            ],
            "contribution_methods": [
                {
                    "method_key": "ScanSignature",
                    "name": "Scan Signature",
                    "title_id": 6001,
                    "description_id": 6002,
                    "progress_description_id": 6003,
                    "rewarding_description_id": 6004,
                    "target_value_description_id": 6005,
                    "icon": "scanning",
                    "unit": 0,
                    "content_tags": ["career_path_explorer"],
                    "parameters": [],
                }
            ],
        }
        out = extract_v24_achievement_career_daily_catalogs(main_pickle, en_labels, fsd_data)
        self.assertEqual(len(out["achievements"]), 1)
        self.assertEqual(out["achievements"][0][10], 3)
        self.assertEqual(out["achievements"][0][11], 138)
        self.assertEqual(len(out["achievement_categories"]), 1)
        self.assertEqual(len(out["career_paths"]), 1)
        self.assertEqual(len(out["daily_goal_catalog"]), 2)
        self.assertEqual(len(out["career_goals"]), 3)
        goals_by_label = {row[4]: row for row in out["career_goals"]}
        self.assertEqual(goals_by_label["EnforcerDestroyNpc100"][10], 100)
        self.assertEqual(goals_by_label["EnforcerSpendMarketIsk1m"][10], 1_000_000)
        self.assertEqual(goals_by_label["ExplorerScanSignature1"][10], 1)


if __name__ == "__main__":
    unittest.main()
