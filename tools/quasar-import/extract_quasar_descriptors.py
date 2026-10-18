#!/usr/bin/env python3
"""
tools/quasar-import/extract_quasar_descriptors.py — V24.01 Quasar FileDescriptorProto Extractor & Codegen.

Extracts all 1,455 serialized `google.protobuf.FileDescriptorProto` binary blobs (`serialized_pb`)
embedded inside the V24.01 EVE client's `code.ccp` (`eveProto/generated/*_pb2.pyj` across 135 domains
listed in `.agents/skills/evomu-adversarial-review/references/parity/sources/quasar_protobuf_catalog.json`).

In compliance with AGENTS.md ("Client-derived data and the public repository"):
- Raw `FileDescriptorProto` blobs are written ONLY to local gitignored `data/quasar_descriptors.sqlite`.
- The tracked `data/quasar_descriptor_manifest.json` records ONLY table/domain/message counts and
  deterministic SHA-256 / MD5 checksums (zero client-derived strings or schema content).
"""

import argparse
import hashlib
import json
import os
import sqlite3
import struct
import sys
import zipfile
import zlib


DEFAULT_SHARED_CACHE = os.path.expanduser(
    "~/Library/Application Support/EVE Online/SharedCache"
)
DEFAULT_CODE_CCP = os.path.join(
    DEFAULT_SHARED_CACHE,
    "tq",
    "EVE.app",
    "Contents",
    "Resources",
    "build",
    "code.ccp",
)


class RawPy27MarshalReader:
    """Minimal Python 2.7 marshal reader that preserves raw `bytes` for string constants."""

    def __init__(self, data: bytes):
        self.data = data
        self.pos = 0
        self.strings: list[bytes] = []

    def read_byte(self) -> int:
        b = self.data[self.pos]
        self.pos += 1
        return b

    def read_int(self) -> int:
        v = struct.unpack_from("<i", self.data, self.pos)[0]
        self.pos += 4
        return v

    def read_int64(self) -> int:
        v = struct.unpack_from("<q", self.data, self.pos)[0]
        self.pos += 8
        return v

    def read_bytes(self, n: int) -> bytes:
        b = self.data[self.pos : self.pos + n]
        self.pos += n
        return b

    def load(self):
        if self.pos >= len(self.data):
            return None
        t = chr(self.read_byte())
        if t in ("0", "N", "F", "T", ".", "S"):
            return None
        if t == "i":
            return self.read_int()
        if t == "I":
            return self.read_int64()
        if t == "l":
            n = self.read_int()
            self.pos += abs(n) * 2
            return 0
        if t == "f":
            n = self.read_byte()
            self.pos += n
            return 0.0
        if t == "g":
            self.pos += 8
            return 0.0
        if t in ("s", "t", "u"):
            n = self.read_int()
            b = self.read_bytes(n)
            if t == "t":
                self.strings.append(b)
            return b
        if t == "R":
            idx = self.read_int()
            return self.strings[idx] if idx < len(self.strings) else b""
        if t in ("(", "[", "{", "<", ">"):
            if t == "{":
                out = {}
                while True:
                    k = self.load()
                    if k is None and chr(self.data[self.pos - 1]) == "0":
                        break
                    out[str(k)] = self.load()
                return out
            n = self.read_int()
            return [self.load() for _ in range(n)]
        if t == "c":
            self.pos += 16  # argcount, nlocals, stacksize, flags
            code_bytes = self.load()
            consts = self.load()
            names = self.load()
            varnames = self.load()
            freevars = self.load()
            cellvars = self.load()
            filename = self.load()
            name = self.load()
            firstlineno = self.read_int()
            lnotab = self.load()
            return {
                "__code__": True,
                "code": code_bytes,
                "consts": consts,
                "names": names,
                "varnames": varnames,
                "freevars": freevars,
                "cellvars": cellvars,
                "filename": filename,
                "name": name,
                "lineno": firstlineno,
                "lnotab": lnotab,
            }
        raise ValueError(f"Unsupported Python 2.7 marshal tag {t!r} at offset {self.pos - 1}")


def read_varint(buf: bytes, pos: int) -> tuple[int, int]:
    res = 0
    shift = 0
    while True:
        b = buf[pos]
        pos += 1
        res |= (b & 0x7F) << shift
        if not (b & 0x80):
            return res, pos
        shift += 7


def parse_pb_fields(buf: bytes) -> list[tuple[int, int, int | bytes]]:
    pos = 0
    n = len(buf)
    fields: list[tuple[int, int, int | bytes]] = []
    while pos < n:
        tag, pos = read_varint(buf, pos)
        field_num = tag >> 3
        wire_type = tag & 0x07
        if wire_type == 0:
            val, pos = read_varint(buf, pos)
        elif wire_type == 1:
            val = buf[pos : pos + 8]
            pos += 8
        elif wire_type == 2:
            ln, pos = read_varint(buf, pos)
            val = buf[pos : pos + ln]
            pos += ln
        elif wire_type == 5:
            val = buf[pos : pos + 4]
            pos += 4
        else:
            raise ValueError(f"Unsupported protobuf wire_type={wire_type} at offset {pos}")
        fields.append((field_num, wire_type, val))
    return fields


def parse_message_stats(buf: bytes) -> tuple[int, int, int]:
    """Returns (total_messages, total_enums, total_fields) in a DescriptorProto subtree."""
    msgs = 1
    enums = 0
    fields = 0
    for fn, _wt, val in parse_pb_fields(buf):
        if fn == 2:
            fields += 1
        elif fn == 3 and isinstance(val, bytes):
            sub_m, sub_e, sub_f = parse_message_stats(val)
            msgs += sub_m
            enums += sub_e
            fields += sub_f
        elif fn == 4:
            enums += 1
    return msgs, enums, fields


def parse_file_descriptor_summary(buf: bytes) -> dict:
    proto_name = ""
    package = ""
    syntax = "proto2"
    dependencies: list[str] = []
    top_messages = 0
    total_messages = 0
    total_enums = 0
    total_services = 0
    total_fields = 0

    for fn, _wt, val in parse_pb_fields(buf):
        if fn == 1 and isinstance(val, bytes):
            proto_name = val.decode("utf-8")
        elif fn == 2 and isinstance(val, bytes):
            package = val.decode("utf-8")
        elif fn == 3 and isinstance(val, bytes):
            dependencies.append(val.decode("utf-8"))
        elif fn == 4 and isinstance(val, bytes):
            top_messages += 1
            sub_m, sub_e, sub_f = parse_message_stats(val)
            total_messages += sub_m
            total_enums += sub_e
            total_fields += sub_f
        elif fn == 5:
            total_enums += 1
        elif fn == 6:
            total_services += 1
        elif fn == 12 and isinstance(val, bytes):
            syntax = val.decode("utf-8")

    return {
        "proto_name": proto_name,
        "package": package,
        "syntax": syntax,
        "dependencies": dependencies,
        "top_messages": top_messages,
        "total_messages": total_messages,
        "total_enums": total_enums,
        "total_services": total_services,
        "total_fields": total_fields,
    }


def extract_serialized_pb_from_pyj(raw_pyj_zlib: bytes, pyj_path: str) -> bytes:
    pyc_payload = zlib.decompress(raw_pyj_zlib)[8:]
    co = RawPy27MarshalReader(pyc_payload).load()
    if not isinstance(co, dict):
        raise ValueError(f"{pyj_path}: top-level marshal object is not a code object")
    consts = co.get("consts") or []
    for idx, c in enumerate(consts):
        if c == b"serialized_pb" and idx + 1 < len(consts) and isinstance(consts[idx + 1], bytes):
            return consts[idx + 1]
    for c in consts:
        if isinstance(c, bytes) and c.startswith(b"\n") and b".proto" in c[:120]:
            return c
    raise ValueError(f"{pyj_path}: no serialized_pb FileDescriptorProto constant found")


def extract_all_descriptors(
    code_ccp_path: str,
    catalog_path: str,
    sqlite_out_path: str,
    manifest_out_path: str | None = None,
) -> dict:
    if not os.path.isfile(code_ccp_path):
        raise FileNotFoundError(
            f"V24.01 client code.ccp archive not found at {code_ccp_path}"
        )
    if not os.path.isfile(catalog_path):
        raise FileNotFoundError(
            f"Quasar protobuf catalog not found at {catalog_path}"
        )

    with open(catalog_path, "r", encoding="utf-8") as f:
        catalog: dict[str, list[str]] = json.load(f)

    os.makedirs(os.path.dirname(os.path.abspath(sqlite_out_path)), exist_ok=True)
    tmp_sqlite = f"{sqlite_out_path}.tmp.{os.getpid()}"
    if os.path.exists(tmp_sqlite):
        os.remove(tmp_sqlite)

    combined_sha256 = hashlib.sha256()
    combined_md5 = hashlib.md5()

    domain_count = len(catalog)
    descriptor_file_count = 0
    unique_packages: set[str] = set()
    top_level_message_count = 0
    total_message_count = 0
    total_enum_count = 0
    total_service_count = 0
    total_field_count = 0
    total_serialized_pb_bytes = 0
    per_domain_file_counts: dict[str, int] = {}

    rows_to_insert = []

    with zipfile.ZipFile(code_ccp_path, "r") as zf:
        for domain in sorted(catalog.keys()):
            paths = sorted(catalog[domain])
            per_domain_file_counts[domain] = len(paths)
            for pyj_path in paths:
                raw_zlib = zf.read(pyj_path)
                serialized_pb = extract_serialized_pb_from_pyj(raw_zlib, pyj_path)
                summary = parse_file_descriptor_summary(serialized_pb)

                pb_sha256 = hashlib.sha256(serialized_pb).hexdigest()
                pb_md5 = hashlib.md5(serialized_pb).hexdigest()

                # Deterministic combined digest over (pyj_path, serialized_pb)
                path_bytes = pyj_path.encode("utf-8")
                header = struct.pack("<I", len(path_bytes)) + path_bytes + struct.pack("<I", len(serialized_pb))
                combined_sha256.update(header)
                combined_sha256.update(serialized_pb)
                combined_md5.update(header)
                combined_md5.update(serialized_pb)

                descriptor_file_count += 1
                unique_packages.add(summary["package"])
                top_level_message_count += summary["top_messages"]
                total_message_count += summary["total_messages"]
                total_enum_count += summary["total_enums"]
                total_service_count += summary["total_services"]
                total_field_count += summary["total_fields"]
                total_serialized_pb_bytes += len(serialized_pb)

                rows_to_insert.append(
                    (
                        pyj_path,
                        domain,
                        summary["proto_name"],
                        summary["package"],
                        summary["syntax"],
                        json.dumps(summary["dependencies"]),
                        pb_sha256,
                        pb_md5,
                        summary["top_messages"],
                        summary["total_messages"],
                        summary["total_enums"],
                        summary["total_services"],
                        summary["total_fields"],
                        serialized_pb,
                    )
                )

    manifest = {
        "schema_version": 1,
        "domain_count": domain_count,
        "descriptor_file_count": descriptor_file_count,
        "unique_package_count": len(unique_packages),
        "top_level_message_count": top_level_message_count,
        "total_message_count": total_message_count,
        "total_enum_count": total_enum_count,
        "total_service_count": total_service_count,
        "total_field_count": total_field_count,
        "total_serialized_pb_bytes": total_serialized_pb_bytes,
        "combined_serialized_pb_sha256": combined_sha256.hexdigest(),
        "combined_serialized_pb_md5": combined_md5.hexdigest(),
        "per_domain_file_counts": per_domain_file_counts,
    }

    conn = sqlite3.connect(tmp_sqlite)
    try:
        conn.execute("PRAGMA journal_mode=DELETE;")
        conn.execute("PRAGMA synchronous=OFF;")
        conn.execute(
            """
            CREATE TABLE quasar_file_descriptors (
                pyj_path TEXT PRIMARY KEY,
                domain TEXT NOT NULL,
                proto_name TEXT NOT NULL,
                package TEXT NOT NULL,
                syntax TEXT NOT NULL,
                dependencies_json TEXT NOT NULL,
                sha256 TEXT NOT NULL,
                md5 TEXT NOT NULL,
                top_message_count INTEGER NOT NULL,
                total_message_count INTEGER NOT NULL,
                enum_count INTEGER NOT NULL,
                service_count INTEGER NOT NULL,
                field_count INTEGER NOT NULL,
                serialized_pb BLOB NOT NULL
            );
            """
        )
        conn.execute(
            """
            CREATE TABLE quasar_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            """
        )
        conn.executemany(
            """
            INSERT INTO quasar_file_descriptors (
                pyj_path, domain, proto_name, package, syntax, dependencies_json,
                sha256, md5, top_message_count, total_message_count, enum_count,
                service_count, field_count, serialized_pb
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);
            """,
            rows_to_insert,
        )
        for k, v in manifest.items():
            conn.execute(
                "INSERT INTO quasar_metadata (key, value) VALUES (?, ?);",
                (k, json.dumps(v) if isinstance(v, dict) else str(v)),
            )
        conn.commit()
    finally:
        conn.close()

    os.replace(tmp_sqlite, sqlite_out_path)

    if manifest_out_path:
        os.makedirs(os.path.dirname(os.path.abspath(manifest_out_path)), exist_ok=True)
        with open(manifest_out_path, "w", encoding="utf-8") as mf:
            json.dump(manifest, mf, indent=2, sort_keys=True)
            mf.write("\n")

    return manifest


def main() -> int:
    repo_root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
    default_catalog = os.path.join(
        repo_root,
        ".agents",
        "skills",
        "evomu-adversarial-review",
        "references",
        "parity",
        "sources",
        "quasar_protobuf_catalog.json",
    )
    default_sqlite = os.path.join(repo_root, "data", "quasar_descriptors.sqlite")
    default_manifest = os.path.join(repo_root, "data", "quasar_descriptor_manifest.json")

    parser = argparse.ArgumentParser(
        description="Extract 1,455 Quasar FileDescriptorProto schemas from V24.01 code.ccp"
    )
    parser.add_argument(
        "--code-ccp",
        default=os.environ.get("EVOMU_CODE_CCP_PATH", DEFAULT_CODE_CCP),
        help="Path to V24.01 client code.ccp archive",
    )
    parser.add_argument(
        "--catalog",
        default=default_catalog,
        help="Path to quasar_protobuf_catalog.json",
    )
    parser.add_argument(
        "--out",
        default=os.environ.get("EVOMU_QUASAR_DESCRIPTORS_PATH", default_sqlite),
        help="Output SQLite database path (gitignored)",
    )
    parser.add_argument(
        "--manifest",
        default=default_manifest,
        help="Output checksum/count manifest JSON path (tracked)",
    )
    parser.add_argument(
        "--no-manifest",
        action="store_true",
        help="Do not overwrite the tracked manifest file",
    )
    args = parser.parse_args()

    manifest_path = None if args.no_manifest else args.manifest
    manifest = extract_all_descriptors(
        code_ccp_path=args.code_ccp,
        catalog_path=args.catalog,
        sqlite_out_path=args.out,
        manifest_out_path=manifest_path,
    )
    print(
        f"[quasar-import] Extracted {manifest['descriptor_file_count']} FileDescriptorProtos "
        f"across {manifest['domain_count']} domains ({manifest['unique_package_count']} packages, "
        f"{manifest['total_message_count']} messages, {manifest['total_enum_count']} enums, "
        f"{manifest['total_service_count']} services, {manifest['total_field_count']} fields, "
        f"sha256={manifest['combined_serialized_pb_sha256']})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
