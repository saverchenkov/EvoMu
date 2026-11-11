#!/usr/bin/env python3
"""Unit and roundtrip tests for belt asteroid SQLite import (`#743`)."""

from __future__ import annotations

import json
import os
import pickle
import sqlite3
import struct
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from import_sde import write_belt_asteroid_tables
from sde_extractors import (
    extract_belt_asteroid_counts,
    extract_system_asteroid_types,
)


def _pack_fsd_dict(items: dict[int, bytes], item_size: int) -> bytes:
    """Packs a minimal FSD dict payload matching `fsd_dict_entries` layout."""
    body = bytearray()
    footer = bytearray(struct.pack("<I", len(items)))
    for k in sorted(items.keys()):
        val = items[k]
        offset = len(body)
        body.extend(val)
        if item_size == 12:
            footer.extend(struct.pack("<III", k, offset, len(val)))
        else:
            footer.extend(struct.pack("<II", k, offset))
    footer_size = len(footer)
    body.extend(footer)
    body.extend(struct.pack("<I", footer_size))
    return struct.pack("<I", len(body)) + bytes(body)


def _build_synthetic_belt_counts_static(data: dict[int, dict[int, int]]) -> bytes:
    schema = b"(dp0\n."
    inner_blobs: dict[int, bytes] = {}
    for belt_id, counts in data.items():
        inner_items = {tid: struct.pack("<I", cnt) for tid, cnt in counts.items()}
        inner_blobs[belt_id] = _pack_fsd_dict(inner_items, item_size=8)
    outer = _pack_fsd_dict(inner_blobs, item_size=12)
    return struct.pack("<I", len(schema)) + schema + outer


class TestBeltAsteroidImport(unittest.TestCase):
    def test_synthetic_extract_and_sqlite_roundtrip(self) -> None:
        raw_static = _build_synthetic_belt_counts_static(
            {
                40000006: {1230: 39, 1228: 27},
                40000003: {1224: 22, 1230: 30},
            }
        )
        belt_counts = extract_belt_asteroid_counts(raw_static)
        self.assertEqual(
            belt_counts,
            [
                (40000003, {1224: 22, 1230: 30}),
                (40000006, {1228: 27, 1230: 39}),
            ],
        )

        raw_pickle = pickle.dumps(
            {
                30000142: (1230, 1228, 1230),
                30000144: (1224, 17459),
            },
            protocol=2,
        )
        sys_types = extract_system_asteroid_types(raw_pickle)
        self.assertEqual(
            sys_types,
            [
                (30000142, [1228, 1230]),
                (30000144, [1224, 17459]),
            ],
        )

        with tempfile.NamedTemporaryFile(suffix=".sqlite") as tf:
            conn = sqlite3.connect(tf.name)
            conn.executescript(
                """
                CREATE TABLE sde_belt_asteroid_counts (
                    belt_id INTEGER PRIMARY KEY,
                    counts_json TEXT NOT NULL
                );
                CREATE TABLE sde_system_asteroid_types (
                    system_id INTEGER PRIMARY KEY,
                    type_ids_json TEXT NOT NULL
                );
                """
            )
            with conn:
                write_belt_asteroid_tables(conn, belt_counts, sys_types)
            rows_b = conn.execute(
                "SELECT belt_id, counts_json FROM sde_belt_asteroid_counts ORDER BY belt_id"
            ).fetchall()
            rows_s = conn.execute(
                "SELECT system_id, type_ids_json FROM sde_system_asteroid_types ORDER BY system_id"
            ).fetchall()
            conn.close()

        self.assertEqual(len(rows_b), 2)
        self.assertEqual(json.loads(rows_b[0][1]), {"1224": 22, "1230": 30})
        self.assertEqual(json.loads(rows_b[1][1]), {"1228": 27, "1230": 39})
        self.assertEqual(len(rows_s), 2)
        self.assertEqual(json.loads(rows_s[0][1]), [1228, 1230])
        self.assertEqual(json.loads(rows_s[1][1]), [1224, 17459])

    def test_real_shared_cache_counts_if_present(self) -> None:
        sc = Path(os.path.expanduser("~/Library/Application Support/EVE Online/SharedCache"))
        static_p = sc / "ResFiles" / "51" / "5129461fccd30475_37125cf3e730bd50495239a6cda04879"
        pickle_p = sc / "ResFiles" / "3f" / "3f555d74faee466f_8f9f9b1e28c0670a443b630c6408c590"
        if not (static_p.exists() and pickle_p.exists()):
            self.skipTest("Local V24 SharedCache belt files not installed")
        belts = extract_belt_asteroid_counts(static_p.read_bytes())
        systems = extract_system_asteroid_types(pickle_p.read_bytes())
        self.assertEqual(len(belts), 40_928)
        self.assertEqual(len(systems), 3_974)


if __name__ == "__main__":
    unittest.main()
